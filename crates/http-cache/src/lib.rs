#![doc = include_str!("../README.md")]

mod control;
mod envelope;

use std::time::Duration;

use bytes::Bytes;
use http::header::{ETAG, IF_NONE_MATCH};
use http::{HeaderValue, Request, Response, StatusCode};
use omnia_sdk::keyvalue::Bucket;
use omnia_sdk::{Error, Result};
use sha2::{Digest, Sha256};

pub use crate::control::Control;
use crate::envelope::{deserialize, serialize};

/// An HTTP response cache over a keyvalue [`Bucket`].
///
/// [`fetch`](Self::fetch) sends a request through [`omnia_sdk::http::fetch`].
/// A request carrying `Cache-Control` is answered from, and written back
/// through, the bucket.
#[derive(Debug)]
pub struct HttpCache {
    bucket: Bucket,
}

impl HttpCache {
    /// A cache storing responses in `bucket`.
    #[must_use]
    pub const fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }

    /// Send `request` to its origin, through the cache when it carries
    /// `Cache-Control`.
    ///
    /// The crate's header contract decides when a stored response is served
    /// and when the origin's is stored.
    ///
    /// # Errors
    ///
    /// - A [`ServerError`](Error::ServerError) with the code `invalid` for
    ///   malformed or conflicting caching headers, before any request leaves.
    /// - The [`BadGateway`](Error::BadGateway) [`omnia_sdk::http::fetch`]
    ///   returns when the origin does not answer.
    pub async fn fetch(&self, mut request: Request<Bytes>) -> Result<Response<Bytes>> {
        // No `Cache-Control`: pure pass-through, headers untouched.
        let Some(control) = Control::maybe_from(request.headers())? else {
            return omnia_sdk::http::fetch(request).await;
        };

        // The etag names the stored entry, through `store_key`.
        let etag = control.etag();
        if control.reads()
            && let Some(hit) = self.read_entry(etag).await
        {
            return Ok(hit);
        }

        // The cache owns conditional semantics: forwarding `If-None-Match`
        // would let the origin answer `304 Not Modified`, whose empty body
        // must not become the cached resource.
        request.headers_mut().remove(IF_NONE_MATCH);
        let mut response = omnia_sdk::http::fetch(request).await?;

        // The request etag identifies the resource the caller asked for, so it
        // replaces whatever the origin sent. `no-store` carries no etag, and
        // RFC 9110 §8.8.3 admits no empty entity-tag, so the origin's `ETag`
        // then stands.
        if !etag.is_empty() {
            let value = HeaderValue::from_str(etag).map_err(|e| {
                Error::server_error("invalid", format!("etag `{etag}` is not a header value: {e}"))
            })?;
            response.headers_mut().insert(ETAG, value);
        }

        // Only complete, successful responses are cacheable: storing a 5xx
        // body would serve it as the resource for `max_age` seconds, and a
        // `206 Partial Content` body would be served as the whole resource.
        // RFC 9111 §3 lets a cache store a 206 only if it understands it,
        // which means the range bookkeeping of §3.3 and §3.4 that this cache
        // does not do.
        if control.writes() && storable(response.status()) {
            self.write_entry(etag, &response, control.max_age()).await;
        }

        Ok(response)
    }
}

// A cache exists to cut response time and bandwidth (RFC 9111 §1); nothing in
// the spec lets its own faults change the answer. So a failing or corrupt read
// is a miss and a failing write hands back the origin response uncached, each
// logged rather than propagated.
impl HttpCache {
    async fn read_entry(&self, etag: &str) -> Option<Response<Bytes>> {
        let hit = match self.bucket.read(&store_key(etag)).await {
            Ok(hit) => hit?,
            Err(e) => {
                tracing::warn!("cache read for etag `{etag}` failed, treating as a miss: {e:#}");
                return None;
            }
        };
        match deserialize(&hit) {
            Ok(response) => {
                tracing::debug!("cache hit for etag `{etag}`");
                Some(response)
            }
            Err(e) => {
                tracing::warn!(
                    "cache entry for etag `{etag}` is corrupt, treating as a miss: {e:#}"
                );
                None
            }
        }
    }

    async fn write_entry(&self, etag: &str, response: &Response<Bytes>, max_age: u64) {
        tracing::debug!("caching response for etag `{etag}`");
        let bytes = match serialize(response) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    "caching response for etag `{etag}` failed, returning it uncached: {e:#}"
                );
                return;
            }
        };
        let ttl = Some(Duration::from_secs(max_age));
        if let Err(e) = self.bucket.write(&store_key(etag), &bytes, ttl).await {
            tracing::warn!(
                "caching response for etag `{etag}` failed, returning it uncached: {e:#}"
            );
        }
    }
}

/// The keyvalue key a response cached under `etag` is stored at.
///
/// The key is `http-cache.` followed by the lower-case hex SHA-256 of `etag`
/// as sent, quotes included. It is a portable key whatever the tag holds, so
/// every backend accepts it.
#[must_use]
pub fn store_key(etag: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut key = String::from("http-cache.");
    for byte in Sha256::digest(etag) {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}

fn storable(status: StatusCode) -> bool {
    status.is_success() && status != StatusCode::PARTIAL_CONTENT
}

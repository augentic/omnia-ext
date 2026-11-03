#![doc = include_str!("../README.md")]

mod control;
mod envelope;

use std::any::Any;
use std::error::Error;

use anyhow::Result;
use bytes::Bytes;
use http::header::{ETAG, IF_NONE_MATCH};
use http::{HeaderValue, Request, Response, StatusCode};
use http_body::Body;
use omnia_sdk::{HttpRequest, StateStore};

pub use crate::control::Control;
use crate::envelope::{deserialize, serialize};

/// Decorates an [`HttpRequest`] so responses are served from, and written
/// back through, a [`StateStore`] when the request carries `Cache-Control`.
#[derive(Clone, Debug, Default)]
pub struct HttpCache<H, S> {
    http: H,
    store: S,
}

impl<H, S> HttpCache<H, S> {
    /// Wrap `http`, storing cacheable responses in `store`.
    #[must_use]
    pub const fn new(http: H, store: S) -> Self {
        Self { http, store }
    }
}

// A cache exists to cut response time and bandwidth (RFC 9111 §1); nothing in
// the spec lets its own faults change the answer. So a failing or corrupt read
// is a miss and a failing write hands back the origin response uncached, each
// logged rather than propagated.
impl<H: HttpRequest, S: StateStore> HttpCache<H, S> {
    /// The stored response under `etag`, or `None` on a miss, a store
    /// failure, or an entry that no longer deserializes.
    async fn read_entry(&self, etag: &str) -> Option<Response<Bytes>> {
        let hit = match self.store.get(etag).await {
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

    /// Store `response` under `etag` for `max_age` seconds, logging rather
    /// than surfacing a serialization or store failure.
    async fn write_entry(&self, etag: &str, response: &Response<Bytes>, max_age: u64) {
        tracing::debug!("caching response for etag `{etag}`");
        let written = match serialize(response) {
            Ok(bytes) => self.store.set(etag, &bytes, Some(max_age)).await,
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            tracing::warn!(
                "caching response for etag `{etag}` failed, returning it uncached: {e:#}"
            );
        }
    }
}

impl<H: HttpRequest, S: StateStore> HttpRequest for HttpCache<H, S> {
    async fn fetch<T>(&self, mut request: Request<T>) -> Result<Response<Bytes>>
    where
        T: Body + Any + Send,
        T::Data: Into<Vec<u8>>,
        T::Error: Into<Box<dyn Error + Send + Sync + 'static>>,
    {
        // No `Cache-Control`: pure pass-through, headers untouched.
        let Some(control) = Control::maybe_from(request.headers())? else {
            return self.http.fetch(request).await;
        };

        // The raw etag is the storage key: no prefix, no hashing.
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
        let mut response = self.http.fetch(request).await?;

        // The request etag identifies the resource the caller asked for, so it
        // replaces whatever the origin sent. `no-store` carries no etag, and
        // RFC 9110 §8.8.3 admits no empty entity-tag, so the origin's `ETag`
        // then stands.
        if !etag.is_empty() {
            response.headers_mut().insert(ETAG, HeaderValue::from_str(etag)?);
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

/// Whether a response with `status` may be stored as the complete resource.
fn storable(status: StatusCode) -> bool {
    status.is_success() && status != StatusCode::PARTIAL_CONTENT
}

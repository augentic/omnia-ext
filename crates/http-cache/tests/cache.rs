//! Tests for the `HttpCache` decorator driven exactly as a guest invokes it,
//! with the outbound request recorded by `MatchedHttp` and the stored copy
//! read back from `Memory`.

// Host-only: the test doubles are `not(wasm32)` dev-dependencies.
#![cfg(not(target_arch = "wasm32"))]

use std::future::{Future, ready};
use std::time::Duration;

use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use omnia_http_cache::{HttpCache, store_key};
use omnia_sdk::capability::{Error, ErrorKind, Result};
use omnia_sdk::keyvalue::Conflict;
use omnia_sdk::{HttpClient as _, StateStore};
use omnia_test::guest::{MatchedHttp, Memory, Provider};

const URL: &str = "https://origin.test/resource";
const ETAG_V1: &str = "\"v1\"";
const NAMESPACED_ETAG: &str = "\"fleet:route=1&stop=2\"";
const ORIGIN_ETAG: &str = "\"origin\"";
const PAYLOAD: &[u8] = b"payload";

/// A provider whose origin answers `GET /resource` with `status` and the
/// fixture body, carrying its own `ETag` so stamping is observable.
fn provider(status: StatusCode) -> Provider {
    let mut response = Response::new(Bytes::from_static(PAYLOAD));
    *response.status_mut() = status;
    response.headers_mut().insert(ETAG, HeaderValue::from_static(ORIGIN_ETAG));
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response.headers_mut().insert(
        http::header::CONTENT_DISPOSITION,
        HeaderValue::from_bytes(b"attachment; filename=\"r\xE9sum\xE9.pdf\"").expect("obs-text"),
    );
    Provider::default().http(MatchedHttp::default().on(Method::GET, URL, response))
}

fn request(headers: &[(http::HeaderName, &'static str)]) -> Request<Bytes> {
    let mut request = Request::get(URL);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    request.body(Bytes::new()).expect("valid request")
}

fn etag(response: &Response<Bytes>) -> Option<&str> {
    response.headers().get(ETAG).map(|v| v.to_str().expect("ascii etag"))
}

/// The stored envelope, as the JSON the crate writes.
fn stored(provider: &Provider) -> Option<serde_json::Value> {
    provider
        .storage
        .state(&store_key(ETAG_V1))
        .map(|bytes| serde_json::from_slice(&bytes).expect("valid JSON"))
}

/// A `StateStore` whose every call fails, standing in for a keyvalue outage.
struct BrokenStore;

impl BrokenStore {
    fn outage<T: Send>() -> impl Future<Output = Result<T>> + Send {
        ready(Err(Error::new(ErrorKind::Unavailable, "store unavailable")))
    }
}

impl StateStore for BrokenStore {
    type Named = Self;

    fn get(&self, _key: &str) -> impl Future<Output = Result<Option<Bytes>>> + Send {
        Self::outage()
    }

    fn set(
        &self, _key: &str, _value: &[u8], _ttl: Option<Duration>,
    ) -> impl Future<Output = Result<()>> + Send {
        Self::outage()
    }

    fn delete(&self, _key: &str) -> impl Future<Output = Result<()>> + Send {
        Self::outage()
    }

    fn compare_and_swap(
        &self, _key: &str, _current: Option<&[u8]>, _new: &[u8], _ttl: Option<Duration>,
    ) -> impl Future<Output = Result<Result<(), Conflict>>> + Send {
        Self::outage()
    }

    fn increment(
        &self, _key: &str, _delta: i64, _ttl: Option<Duration>,
    ) -> impl Future<Output = Result<i64>> + Send {
        Self::outage()
    }

    fn named(&self, _name: &str) -> Self {
        Self
    }
}

/// A `StateStore` over `Memory` that, like NATS, refuses a key the
/// `StateStore` contract does not call portable.
#[derive(Default)]
struct PortableStore(Memory);

impl PortableStore {
    fn checked(key: &str) -> Result<()> {
        let portable = !key.is_empty()
            && !key.starts_with('.')
            && !key.ends_with('.')
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !portable {
            return Err(Error::new(ErrorKind::Invalid, format!("`{key}` is not a portable key")));
        }
        Ok(())
    }
}

impl StateStore for PortableStore {
    type Named = Self;

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        Self::checked(key)?;
        self.0.get(key).await
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        Self::checked(key)?;
        self.0.set(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        Self::checked(key)?;
        self.0.delete(key).await
    }

    async fn compare_and_swap(
        &self, key: &str, current: Option<&[u8]>, new: &[u8], ttl: Option<Duration>,
    ) -> Result<Result<(), Conflict>> {
        Self::checked(key)?;
        self.0.compare_and_swap(key, current, new, ttl).await
    }

    async fn increment(&self, key: &str, delta: i64, ttl: Option<Duration>) -> Result<i64> {
        Self::checked(key)?;
        self.0.increment(key, delta, ttl).await
    }

    fn named(&self, name: &str) -> Self {
        Self(self.0.named(name))
    }
}

#[tokio::test]
async fn max_age_miss_then_hit() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    let miss = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(miss.status(), StatusCode::OK);
    assert_eq!(miss.body(), PAYLOAD);
    assert_eq!(etag(&miss), Some(ETAG_V1));

    // The conditional header stays with the cache: the origin must answer
    // with the full resource, never a 304.
    let requests = provider.http.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key(IF_NONE_MATCH));

    // The stored copy, under the etag's `store_key`, is the stamped response
    // rather than the origin's. Header values are stored as bytes so nothing
    // RFC 9110 allows in a field value is lost.
    let envelope = stored(&provider).expect("response cached");
    assert_eq!(envelope["status"], 200);
    assert_eq!(envelope["body"], serde_json::json!(PAYLOAD));
    assert!(
        envelope["headers"]
            .as_array()
            .expect("headers array")
            .contains(&serde_json::json!(["etag", ETAG_V1.as_bytes()]))
    );

    let hit = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(hit.status(), StatusCode::OK);
    assert_eq!(hit.body(), PAYLOAD);
    assert_eq!(etag(&hit), Some(ETAG_V1));
    assert_eq!(
        hit.headers().get(CONTENT_TYPE).map(HeaderValue::as_bytes),
        Some(b"text/plain".as_slice())
    );
    assert_eq!(
        hit.headers().get(http::header::CONTENT_DISPOSITION).map(HeaderValue::as_bytes),
        Some(b"attachment; filename=\"r\xE9sum\xE9.pdf\"".as_slice())
    );
    assert_eq!(provider.http.requests().len(), 1);
}

#[tokio::test]
async fn no_cache_max_age() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "no-cache, max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    for round in 1..=2 {
        // A stale entry under the key must be bypassed and then replaced.
        provider.storage.insert_state(&store_key(ETAG_V1), b"stale");

        let response = cache.fetch(request(&headers)).await.expect("should succeed");
        assert_eq!(response.body(), PAYLOAD);
        assert_eq!(etag(&response), Some(ETAG_V1));
        assert_eq!(provider.http.requests().len(), round);
        assert_eq!(stored(&provider).expect("response cached")["status"], 200);
    }
}

#[tokio::test]
async fn multiline_cache_control() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    // Two `.header()` calls: one list per RFC 9110 §5.3, so this is the
    // `no-cache, max-age=60` refresh, not a bare `no-cache` bypass.
    let headers =
        [(CACHE_CONTROL, "no-cache"), (CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];
    provider.storage.insert_state(&store_key(ETAG_V1), b"stale");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(provider.http.requests().len(), 1);
    assert_eq!(stored(&provider).expect("response cached")["status"], 200);

    // And a `no-store` on a later line is seen, so the conflict is refused
    // before any request leaves.
    let headers =
        [(CACHE_CONTROL, "max-age=60"), (CACHE_CONTROL, "no-store"), (IF_NONE_MATCH, ETAG_V1)];
    let Err(err) = cache.fetch(request(&headers)).await else {
        panic!("expected conflicting directives error");
    };
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert_eq!(provider.http.requests().len(), 1);
}

#[tokio::test]
async fn no_cache() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "no-cache"), (IF_NONE_MATCH, ETAG_V1)];

    // RFC 9111 §5.2.1.4: the stored copy must not be served unvalidated, but
    // with no `max-age` there is no lifetime to write the response under, so
    // the existing entry is left as it was.
    provider.storage.insert_state(&store_key(ETAG_V1), b"existing");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);
    assert_eq!(
        provider.storage.state(&store_key(ETAG_V1)).as_deref(),
        Some(b"existing".as_slice())
    );
}

#[tokio::test]
async fn zero_max_age() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=0"), (IF_NONE_MATCH, ETAG_V1)];

    // RFC 9111 §5.2.1.1: `max-age=0` accepts no stored response, however
    // fresh, so a populated entry is bypassed rather than served.
    provider.storage.insert_state(&store_key(ETAG_V1), b"stale");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);

    // Nor is there a lifetime to store the fresh copy under.
    assert_eq!(provider.storage.state(&store_key(ETAG_V1)).as_deref(), Some(b"stale".as_slice()));
}

#[tokio::test]
async fn no_store() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "no-store"), (IF_NONE_MATCH, ETAG_V1)];

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(provider.http.requests().len(), 1);
    assert!(stored(&provider).is_none());

    // `Control` records no etag under `no-store`, and an empty entity-tag is
    // not valid (RFC 9110 §8.8.3), so the origin's `ETag` is left in place.
    assert_eq!(etag(&response), Some(ORIGIN_ETAG));
}

#[tokio::test]
async fn server_error() {
    let provider = provider(StatusCode::INTERNAL_SERVER_ERROR);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);
    assert!(stored(&provider).is_none());
}

#[tokio::test]
async fn partial_content() {
    let provider = provider(StatusCode::PARTIAL_CONTENT);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    // RFC 9111 §3: a 206 is storable only by a cache that understands range
    // combination. This one does not, so the ranged body is returned to the
    // caller but never keyed as the whole resource.
    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);
    assert!(stored(&provider).is_none());
}

#[tokio::test]
async fn corrupt_entry() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    // Whatever is under the key is not an envelope this crate wrote.
    provider.storage.insert_state(&store_key(ETAG_V1), b"not an envelope");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);

    // The origin response overwrites the corrupt entry and serves the next call.
    assert_eq!(stored(&provider).expect("response cached")["status"], 200);
    let hit = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(hit.body(), PAYLOAD);
    assert_eq!(provider.http.requests().len(), 1);
}

#[tokio::test]
async fn store_outage() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, BrokenStore);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    // The read fails, the origin is asked, the write fails: the caller still
    // gets the response the origin produced, on every call.
    for round in 1..=2 {
        let response = cache.fetch(request(&headers)).await.expect("should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body(), PAYLOAD);
        assert_eq!(etag(&response), Some(ETAG_V1));
        assert_eq!(provider.http.requests().len(), round);
    }
}

#[tokio::test]
async fn namespaced_etag() {
    let provider = provider(StatusCode::OK);
    let store = PortableStore::default();
    let cache = HttpCache::new(&provider, &store);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, NAMESPACED_ETAG)];

    // The tag's quotes, `:`, `=`, and `&` are not portable key characters, so
    // a store that refuses such a key still serves the miss's entry as a hit.
    for _ in 1..=2 {
        let response = cache.fetch(request(&headers)).await.expect("should succeed");
        assert_eq!(response.body(), PAYLOAD);
        assert_eq!(etag(&response), Some(NAMESPACED_ETAG));
        assert_eq!(provider.http.requests().len(), 1);
    }
    assert!(store.0.state(&store_key(NAMESPACED_ETAG)).is_some());
}

#[tokio::test]
async fn absent_cache_control() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    let response = cache.fetch(request(&[(IF_NONE_MATCH, ETAG_V1)])).await.expect("should succeed");
    assert_eq!(etag(&response), Some(ORIGIN_ETAG));

    let requests = provider.http.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers.get(IF_NONE_MATCH).map(HeaderValue::as_bytes),
        Some(ETAG_V1.as_bytes())
    );
    assert!(stored(&provider).is_none());
}

#[tokio::test]
async fn unknown_directives() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    // RFC 9111 §5.2.3: nothing here is a directive this cache acts on, so the
    // request is forwarded exactly as sent, `If-None-Match` included, and the
    // origin's `ETag` is not rewritten.
    let headers = [(CACHE_CONTROL, "no-transform"), (IF_NONE_MATCH, ETAG_V1)];
    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(etag(&response), Some(ORIGIN_ETAG));

    let requests = provider.http.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers.get(IF_NONE_MATCH).map(HeaderValue::as_bytes),
        Some(ETAG_V1.as_bytes())
    );
    assert!(stored(&provider).is_none());

    // And without an etag it is not an error either.
    cache.fetch(request(&[(CACHE_CONTROL, "no-transform")])).await.expect("should succeed");
    assert_eq!(provider.http.requests().len(), 2);
}

#[tokio::test]
async fn bad_directives() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    for headers in [
        vec![(CACHE_CONTROL, "max-age=60")],
        vec![(CACHE_CONTROL, "max-age=60, max-age=0"), (IF_NONE_MATCH, ETAG_V1)],
        vec![(CACHE_CONTROL, "no-store, max-age=0"), (IF_NONE_MATCH, ETAG_V1)],
        vec![(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, "W/\"weak\"")],
        vec![(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, "\"a\", \"b\"")],
        vec![(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, "*")],
        vec![(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, "unquoted")],
    ] {
        let Err(err) = cache.fetch(request(&headers)).await else {
            panic!("expected malformed directives to be refused: {headers:?}");
        };
        assert_eq!(err.kind(), ErrorKind::Invalid, "{headers:?}");
    }
    assert!(provider.http.requests().is_empty());
    assert!(stored(&provider).is_none());
}

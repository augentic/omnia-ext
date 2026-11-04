//! Tests for the `HttpCache` decorator driven exactly as a guest invokes it,
//! with the outbound request recorded by `MatchedHttp` and the stored copy
//! read back from `Memory`.

// Host-only: the test doubles are `not(wasm32)` dev-dependencies.
#![cfg(not(target_arch = "wasm32"))]

use std::future::{Future, ready};

use anyhow::{Result, anyhow};
use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::Empty;
use omnia_http_cache::HttpCache;
use omnia_sdk::{CasError, HttpRequest as _, StateStore};
use omnia_test::guest::{MatchedHttp, Provider};

const URL: &str = "https://origin.test/resource";
const ETAG_V1: &str = "\"v1\"";
const ORIGIN_ETAG: &str = "\"origin\"";
const PAYLOAD: &[u8] = b"payload";

/// A provider whose origin answers `GET /resource` with `status` and the
/// fixture body, carrying its own `ETag` so stamping is observable.
fn provider(status: StatusCode) -> Provider {
    let mut response = Response::new(Bytes::from_static(PAYLOAD));
    *response.status_mut() = status;
    response.headers_mut().insert(ETAG, HeaderValue::from_static(ORIGIN_ETAG));
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    Provider::default().http(MatchedHttp::default().on(Method::GET, URL, response))
}

fn request(headers: &[(http::HeaderName, &'static str)]) -> Request<Empty<Bytes>> {
    let mut request = Request::get(URL);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    request.body(Empty::new()).expect("valid request")
}

fn etag(response: &Response<Bytes>) -> Option<&str> {
    response.headers().get(ETAG).map(|v| v.to_str().expect("ascii etag"))
}

/// The stored envelope, as the JSON the crate writes.
fn stored(provider: &Provider) -> Option<serde_json::Value> {
    provider.storage.state(ETAG_V1).map(|bytes| serde_json::from_slice(&bytes).expect("valid JSON"))
}

/// A `StateStore` whose every call fails, standing in for a keyvalue outage.
struct BrokenStore;

impl StateStore for BrokenStore {
    fn get(&self, _key: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send {
        ready(Err(anyhow!("store unavailable")))
    }

    fn set(
        &self, _key: &str, _value: &[u8], _ttl_secs: Option<u64>,
    ) -> impl Future<Output = Result<()>> + Send {
        ready(Err(anyhow!("store unavailable")))
    }

    fn delete(&self, _key: &str) -> impl Future<Output = Result<()>> + Send {
        ready(Err(anyhow!("store unavailable")))
    }

    fn cas(
        &self, _key: &str, _expected: Option<&[u8]>, _value: &[u8],
    ) -> impl Future<Output = Result<(), CasError>> + Send {
        ready(Err(CasError::Store("store unavailable".into())))
    }

    fn increment(&self, _key: &str, _delta: i64) -> impl Future<Output = Result<i64>> + Send {
        ready(Err(anyhow!("store unavailable")))
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

    // The stored copy is keyed by the raw etag, quotes included, and is the
    // stamped response rather than the origin's. Header values are stored as
    // bytes so nothing RFC 9110 allows in a field value is lost.
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
    assert_eq!(provider.http.requests().len(), 1);
}

#[tokio::test]
async fn no_cache_refreshes() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "no-cache, max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    for round in 1..=2 {
        // A stale entry under the key must be bypassed and then replaced.
        provider.storage.insert_state(ETAG_V1, b"stale");

        let response = cache.fetch(request(&headers)).await.expect("should succeed");
        assert_eq!(response.body(), PAYLOAD);
        assert_eq!(etag(&response), Some(ETAG_V1));
        assert_eq!(provider.http.requests().len(), round);
        assert_eq!(stored(&provider).expect("response cached")["status"], 200);
    }
}

#[tokio::test]
async fn cache_control_split_across_lines() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    // Two `.header()` calls: one list per RFC 9110 §5.3, so this is the
    // `no-cache, max-age=60` refresh, not a bare `no-cache` bypass.
    let headers =
        [(CACHE_CONTROL, "no-cache"), (CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];
    provider.storage.insert_state(ETAG_V1, b"stale");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(provider.http.requests().len(), 1);
    assert_eq!(stored(&provider).expect("response cached")["status"], 200);

    // And a `no-store` on a later line is seen, so the conflict is refused
    // before any request leaves.
    let headers =
        [(CACHE_CONTROL, "max-age=60"), (CACHE_CONTROL, "no-store"), (IF_NONE_MATCH, ETAG_V1)];
    let Err(_) = cache.fetch(request(&headers)).await else {
        panic!("expected conflicting directives error");
    };
    assert_eq!(provider.http.requests().len(), 1);
}

#[tokio::test]
async fn no_cache_alone_bypasses_without_writing() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "no-cache"), (IF_NONE_MATCH, ETAG_V1)];

    // RFC 9111 §5.2.1.4: the stored copy must not be served unvalidated, but
    // with no `max-age` there is no lifetime to write the response under, so
    // the existing entry is left as it was.
    provider.storage.insert_state(ETAG_V1, b"existing");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);
    assert_eq!(provider.storage.state(ETAG_V1).as_deref(), Some(b"existing".as_slice()));
}

#[tokio::test]
async fn zero_max_age_revalidates() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=0"), (IF_NONE_MATCH, ETAG_V1)];

    // RFC 9111 §5.2.1.1: `max-age=0` accepts no stored response, however
    // fresh, so a populated entry is bypassed rather than served.
    provider.storage.insert_state(ETAG_V1, b"stale");

    let response = cache.fetch(request(&headers)).await.expect("should succeed");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body(), PAYLOAD);
    assert_eq!(etag(&response), Some(ETAG_V1));
    assert_eq!(provider.http.requests().len(), 1);

    // Nor is there a lifetime to store the fresh copy under.
    assert_eq!(provider.storage.state(ETAG_V1).as_deref(), Some(b"stale".as_slice()));
}

#[tokio::test]
async fn no_store_bypasses() {
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
async fn non_success_not_stored() {
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
async fn partial_content_not_stored() {
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
async fn corrupt_entry_is_a_miss_and_replaced() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);
    let headers = [(CACHE_CONTROL, "max-age=60"), (IF_NONE_MATCH, ETAG_V1)];

    // Whatever is under the key is not an envelope this crate wrote.
    provider.storage.insert_state(ETAG_V1, b"not an envelope");

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
async fn store_outage_degrades_to_origin() {
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
async fn without_cache_control_passes_through() {
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
async fn unrecognised_directives_pass_through() {
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
async fn malformed_directives_refused() {
    let provider = provider(StatusCode::OK);
    let cache = HttpCache::new(&provider, &provider);

    let Err(_) = cache.fetch(request(&[(CACHE_CONTROL, "max-age=60")])).await else {
        panic!("expected missing If-None-Match error");
    };
    assert!(provider.http.requests().is_empty());
    assert!(stored(&provider).is_none());
}

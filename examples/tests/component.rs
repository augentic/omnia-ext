//! Component rung: the example guest, compiled to a component, through the
//! example host's own wiring.
//!
//! `build.rs` compiles `http-cache/guest.rs` to a `wasm32-wasip2` component
//! and `gen.rs` names it (`HTTP_CACHE_WASM`). `http-cache/runtime.rs` is
//! included as `production`, so the `Hooks` its `runtime!` generates — the
//! exact host rows the example binary links — assemble the runtime over
//! `omnia_test::host::Backends`, the in-memory defaults. A host the guest
//! imports but the example does not declare fails here at link time, which
//! the handler rung in `crates/http-cache/tests` cannot see.
//!
//! The scenario drives the `wasi:http` export in-process through
//! [`HttpHandler`], bypassing the socket, against a test-owned loopback
//! origin, and asserts what reached the origin, what the handler answered,
//! and what the guest's `StateStore` persisted through the example's
//! `wasi:keyvalue` host. Header semantics stay with the handler rung.

#![cfg(not(target_arch = "wasm32"))]

use std::convert::Infallible;
use std::future::ready;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::header::{ETAG, HOST, IF_NONE_MATCH};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use omnia_http_cache::store_key;
use omnia_test::host::{Backends, Deployment};
use omnia_wasi_http::HttpHandler;
use serde_json::Value;
use tokio::net::TcpListener;

include!(concat!(env!("OUT_DIR"), "/gen.rs"));

// The production `runtime!` as the example binary compiles it, untouched:
// `Hooks` is the wiring under test, `manifest()` the (empty) deployment it
// compiles in.
#[path = "../http-cache/runtime.rs"]
mod production;

/// The body the origin answers every request with.
const BODY: &[u8] = b"hello from origin";

/// The etag the guest sends, whose `store_key` the response is stored under.
const ETAG_V1: &str = "\"v1\"";

/// One request as the origin saw it.
#[derive(Clone, Debug)]
struct Hit {
    path: String,
    headers: HeaderMap,
}

/// A loopback origin answering every request with `200`, its own `ETag`, and
/// [`BODY`] — honouring nothing conditional — and recording each hit.
#[derive(Clone, Debug, Default)]
struct Origin {
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Origin {
    /// Serves a fresh origin on an ephemeral port for the life of the test
    /// runtime; returns it with its base URL.
    async fn serve() -> (Self, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binding loopback");
        let addr = listener.local_addr().expect("listener address");
        let origin = Self::default();

        let accepting = origin.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let origin = accepting.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        ready(Ok::<_, Infallible>(origin.answer(&request)))
                    });
                    let _ =
                        http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });

        (origin, format!("http://{addr}"))
    }

    fn answer(&self, request: &Request<Incoming>) -> Response<Full<Bytes>> {
        self.hits.lock().expect("hits lock").push(Hit {
            path: request.uri().path().to_owned(),
            headers: request.headers().clone(),
        });
        // A different etag than the guest's: stamping is observable.
        Response::builder()
            .header(ETAG, "\"origin\"")
            .body(Full::new(Bytes::from_static(BODY)))
            .expect("origin response")
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().expect("hits lock").clone()
    }
}

/// A JSON array of byte values as bytes.
fn bytes(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .expect("byte array")
        .iter()
        .map(|byte| u8::try_from(byte.as_u64().expect("byte")).expect("byte range"))
        .collect()
}

/// `GET /cached` twice: the first reaches the origin and is stored, the
/// second is answered from the store.
#[tokio::test]
async fn cached_miss_then_hit() {
    // The generated `main` and `run` stay untouched; only `Hooks` is driven.
    let _ = (production::main, production::run);
    let (origin, url) = Origin::serve().await;
    let backends = Backends::defaults().await.config([("UPSTREAM_URL", url)]);
    let runtime = Deployment::from(production::manifest())
        .guest("guest", HTTP_CACHE_WASM)
        .boot(backends.clone(), <production::Hooks as omnia::Wiring<Backends>>::link)
        .await
        .expect("the example guest links through the example host's wiring");
    let handler = HttpHandler::new(&runtime)
        .expect("http routes consistent")
        .expect("the guest exports the http handler");

    for _ in 0..2 {
        // `Host` is required: the handler answers a request without an
        // authority with `400` before the guest sees it.
        let request = Request::get("/cached")
            .header(HOST, "guest.test")
            .body(Full::new(Bytes::new()))
            .expect("request");
        let response = handler.handle(request).await.expect("handled");
        let (parts, body) = response.into_parts();
        let body = body.collect().await.expect("body streams").to_bytes();
        assert_eq!(parts.status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        // The request etag, not the origin's, on the miss and the hit alike.
        assert_eq!(parts.headers.get(ETAG).map(HeaderValue::as_bytes), Some(ETAG_V1.as_bytes()));
        assert_eq!(body, BODY);
    }

    // The second GET never left the guest, and the one that did carried no
    // conditional header: the cache owns those semantics.
    let hits = origin.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/resource");
    assert!(hits[0].headers.get(IF_NONE_MATCH).is_none(), "If-None-Match is not forwarded");

    // Persisted state: the bucket the guest's `StateStore` opens holds the
    // serialized response under the etag's `store_key`, with TTL enforced
    // natively by the bucket.
    let entry = backends
        .state(&store_key(ETAG_V1))
        .await
        .expect("the response is stored under the etag's key");
    let cached: Value = serde_json::from_slice(&entry).expect("serialized response");
    assert_eq!(cached["status"], 200);
    assert_eq!(bytes(&cached["body"]), BODY);

    runtime.shutdown();
}

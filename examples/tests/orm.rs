//! Component rung: the ORM example guest, compiled to a component, through
//! the example host's own wiring.
//!
//! `build.rs` compiles `orm/guest.rs` to a `wasm32-wasip2` component and
//! `gen.rs` names it (`ORM_WASM`). `orm/runtime.rs` is included as
//! `production`, so the `Hooks` its `runtime!` generates — the exact host
//! rows the example binary links — assemble the runtime over
//! `omnia_test::host::Backends`, whose `omnia:sql` and `wasi:sql` share one
//! private in-memory `SQLite` that lives for the whole boot. A host the
//! guest imports but the example does not declare fails here at link time,
//! which the handler rung in `crates/orm/tests` cannot see.
//!
//! One scenario chains the routes in-process through [`HttpHandler`],
//! bypassing the socket: rows written by one request are read back by the
//! next, through the JOIN entity, and gone after the delete. SQL text and
//! parameter shapes stay with the handler rung.

#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HOST};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Full};
use omnia_test::host::{Backends, Deployment};
use omnia_wasi_http::HttpHandler;
use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/gen.rs"));

// The production `runtime!` as the example binary compiles it, untouched:
// `Hooks` is the wiring under test, `manifest()` the (empty) deployment it
// compiles in.
#[path = "../orm/runtime.rs"]
mod production;

/// Sends `method path` with an optional JSON body and returns the status and
/// the decoded JSON reply (or `Null` when the body is not JSON).
async fn call(
    handler: &HttpHandler<Backends>, method: Method, path: &str, body: Option<Value>,
) -> (StatusCode, Value) {
    // `Host` is required: the handler answers a request without an authority
    // with `400` before the guest sees it.
    let mut request = Request::builder().method(method).uri(path).header(HOST, "guest.test");
    let bytes = match body {
        Some(json) => {
            request = request.header(CONTENT_TYPE, "application/json");
            Bytes::from(serde_json::to_vec(&json).expect("json body"))
        }
        None => Bytes::new(),
    };
    let request = request.body(Full::new(bytes)).expect("request");

    let response = handler.handle(request).await.expect("handled");
    let (parts, body) = response.into_parts();
    let body = body.collect().await.expect("body streams").to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (parts.status, json)
}

/// Create an agency and a feed, read the feed back through the JOIN entity,
/// delete it, and read an empty list.
#[tokio::test]
async fn agency_feed_lifecycle() {
    // The generated `main` and `run` stay untouched; only `Hooks` is driven.
    let _ = (production::main, production::run);
    let runtime = Deployment::from(production::manifest())
        .guest("guest", ORM_WASM)
        .boot(Backends::defaults().await, <production::Hooks as omnia::Wiring<Backends>>::link)
        .await
        .expect("the example guest links through the example host's wiring");
    let handler = HttpHandler::new(&runtime)
        .expect("http routes consistent")
        .expect("the guest exports the http handler");

    let agency = json!({
        "agency_id": 1,
        "name": "Ritchies Transport",
        "url": "https://ritchies.test",
        "timezone": "Pacific/Auckland",
    });
    let (status, reply) = call(&handler, Method::POST, "/agencies", Some(agency)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["agency"]["agency_id"], 1);
    assert_eq!(reply["agency"]["name"], "Ritchies Transport");

    // A later request, a fresh guest instance, still sees the row: the
    // backend's SQLite outlives the instance.
    let (status, reply) = call(&handler, Method::GET, "/agencies", None).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["agencies"].as_array().map(Vec::len), Some(1));
    assert_eq!(reply["agencies"][0]["timezone"], "Pacific/Auckland");

    let update = json!({ "name": "Ritchies", "timezone": "Pacific/Auckland" });
    let (status, reply) = call(&handler, Method::PATCH, "/agencies/1", Some(update)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    let feed = json!({ "feed_id": 1, "description": "Bus routes and schedules" });
    let (status, reply) = call(&handler, Method::POST, "/agencies/1/feeds", Some(feed)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["feed"]["agency_id"], 1);

    // The JOIN entity carries the agency's (updated) name alongside the feed.
    let (status, reply) = call(&handler, Method::GET, "/feeds", None).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let feeds = reply["feeds"].as_array().expect("feeds array");
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0]["feed_id"], 1);
    assert_eq!(feeds[0]["description"], "Bus routes and schedules");
    assert_eq!(feeds[0]["agency_name"], "Ritchies");

    let (status, reply) = call(&handler, Method::DELETE, "/feeds/1", None).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["feed_id"], 1);

    let (status, reply) = call(&handler, Method::GET, "/feeds", None).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["feeds"], json!([]));

    // Zero rows affected is the guest's not-found path; the SDK maps an
    // `anyhow` error to `500`.
    let (status, _) = call(&handler, Method::DELETE, "/feeds/1", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    runtime.shutdown();
}

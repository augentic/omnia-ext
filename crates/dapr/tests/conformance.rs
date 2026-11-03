//! Conformance and fake-sidecar end-to-end suite for `omnia_dapr::Dapr`.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use http::header::{CONTENT_TYPE, ETAG};
use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use omnia_backend::http::ConnectOptions as HttpOptions;
use omnia_backend::{Connect as _, HttpDefault};
use omnia_dapr::Dapr;
use omnia_dapr::axum::subscriptions;
use omnia_native::Direct;
use omnia_sdk::api::messaging::{Router as MessagingRouter, consume};
use omnia_sdk::api::{Client, Context};
use omnia_sdk::{BlobStore, BlobStoreExt as _, Error, StateStore};
use omnia_test::conformance;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt as _;

omnia_test::conformance!(state, publish, config);

#[derive(Clone, Default)]
struct FakeSidecar {
    inner: Arc<Mutex<SidecarState>>,
}

#[derive(Default)]
struct SidecarState {
    next_etag: u64,
    state: HashMap<String, StoredItem>,
    blobs: HashMap<String, HashMap<String, String>>,
    published: Vec<PublishedEvent>,
    config: HashMap<String, String>,
}

#[derive(Clone)]
struct StoredItem {
    value: Value,
    etag: String,
    expires_at: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishedEvent {
    pubsub: String,
    topic: String,
    query: Option<String>,
    payload: Vec<u8>,
}

#[derive(Deserialize)]
struct StateWrite {
    key: String,
    value: Value,
    etag: Option<String>,
    metadata: Option< HashMap<String, String>>,
    options: Option<WriteOptions>,
}

#[derive(Deserialize)]
struct WriteOptions {
    concurrency: Option<String>,
}

#[derive(Deserialize)]
struct BindingCall {
    operation: String,
    data: Option<String>,
    metadata: Option<HashMap<String, String>>,
}

impl FakeSidecar {
    async fn serve() -> (Self, Dapr<Direct<HttpDefault>>) {
        let sidecar = Self::default();
        sidecar
            .inner
            .lock()
            .expect("sidecar lock")
            .config
            .insert("CONFORMANCE_KEY".to_owned(), "conformance-val".to_owned());

        let app = Router::new()
            .route("/v1.0/state/{store}", post(state_post))
            .route("/v1.0/state/{store}/{*key}", get(state_get).delete(state_delete))
            .route("/v1.0/publish/{pubsub}/{*topic}", post(publish_post))
            .route("/v1.0/configuration/{store}", get(config_get_route))
            .route("/v1.0/bindings/{container}", post(binding_post))
            .with_state(sidecar.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake sidecar");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let http = Direct::new(
            HttpDefault::connect_with(HttpOptions { connect_timeout: 5 }).await.expect("http"),
        );
        let dapr = Dapr::new(http).endpoint(format!("http://{addr}"));
        (sidecar, dapr)
    }

    fn published(&self) -> Vec<PublishedEvent> {
        self.inner.lock().expect("sidecar lock").published.clone()
    }
}

fn live_entry<'a>(
    state: &'a mut HashMap<String, StoredItem>, scoped_key: &str,
) -> Option<&'a StoredItem> {
    if state
        .get(scoped_key)
        .and_then(|item| item.expires_at)
        .is_some_and(|deadline| deadline <= Instant::now())
    {
        state.remove(scoped_key);
        return None;
    }
    state.get(scoped_key)
}

async fn state_get(
    State(sidecar): State<FakeSidecar>, Path((store, key)): Path<(String, String)>,
) -> Response<Full<Bytes>> {
    let key = key.trim_start_matches('/');
    let scoped = format!("{store}||{key}");
    let mut guard = sidecar.inner.lock().expect("sidecar lock");
    let Some(item) = live_entry(&mut guard.state, &scoped).cloned() else {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Full::new(Bytes::new()))
            .expect("response");
    };
    let body = serde_json::to_vec(&item.value).expect("serialize state value");
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .header(ETAG, format!("\"{}\"", item.etag))
        .body(Full::new(Bytes::from(body)))
        .expect("response")
}

async fn state_post(
    State(sidecar): State<FakeSidecar>, Path(store): Path<String>, Json(items): Json<Vec<StateWrite>>,
) -> StatusCode {
    let mut guard = sidecar.inner.lock().expect("sidecar lock");
    for item in items {
        let scoped = format!("{store}||{}", item.key);
        let first_write = item
            .options
            .as_ref()
            .and_then(|o| o.concurrency.as_deref())
            == Some("first-write");

        if first_write {
            let current_etag = live_entry(&mut guard.state, &scoped).map(|e| e.etag.clone());
            match (item.etag.as_deref(), current_etag.as_deref()) {
                (Some("") | None, None) => {}
                (Some(expected), Some(actual)) if !expected.is_empty() && expected == actual => {}
                _ => return StatusCode::CONFLICT,
            }
        }

        let expires_at = item
            .metadata
            .as_ref()
            .and_then(|m| m.get("ttlInSeconds"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|secs| Instant::now() + Duration::from_secs(secs));

        guard.next_etag += 1;
        let etag = guard.next_etag.to_string();
        guard.state.insert(
            scoped,
            StoredItem {
                value: item.value,
                etag,
                expires_at,
            },
        );
    }
    StatusCode::NO_CONTENT
}

async fn state_delete(
    State(sidecar): State<FakeSidecar>, Path((store, key)): Path<(String, String)>,
) -> StatusCode {
    let key = key.trim_start_matches('/');
    let scoped = format!("{store}||{key}");
    sidecar.inner.lock().expect("sidecar lock").state.remove(&scoped);
    StatusCode::NO_CONTENT
}

async fn publish_post(
    State(sidecar): State<FakeSidecar>, Path((pubsub, topic)): Path<(String, String)>,
    RawQuery(query): RawQuery, body: Bytes,
) -> StatusCode {
    let topic = topic.trim_start_matches('/').to_owned();
    sidecar.inner.lock().expect("sidecar lock").published.push(PublishedEvent {
        pubsub,
        topic,
        query,
        payload: body.to_vec(),
    });
    StatusCode::NO_CONTENT
}

async fn config_get_route(
    State(sidecar): State<FakeSidecar>, Path(_store): Path<String>, RawQuery(query): RawQuery,
) -> Json<Value> {
    let key = query
        .as_deref()
        .and_then(|q| q.strip_prefix("key="))
        .unwrap_or_default();
    let guard = sidecar.inner.lock().expect("sidecar lock");
    let mut map = serde_json::Map::new();
    if let Some(val) = guard.config.get(key) {
        map.insert(key.to_owned(), json!({ "value": val }));
    }
    Json(Value::Object(map))
}

async fn binding_post(
    State(sidecar): State<FakeSidecar>, Path(container): Path<String>, Json(call): Json<BindingCall>,
) -> Response<Full<Bytes>> {
    let mut guard = sidecar.inner.lock().expect("sidecar lock");
    let bucket = guard.blobs.entry(container).or_default();
    let key = call
        .metadata
        .as_ref()
        .and_then(|m| m.get("key").or_else(|| m.get("blobName")))
        .cloned()
        .unwrap_or_default();

    match call.operation.as_str() {
        "create" => {
            bucket.insert(key, call.data.unwrap_or_default());
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .expect("response")
        }
        "get" => match bucket.get(&key) {
            Some(encoded) => {
                let body = serde_json::to_vec(encoded).expect("serialize blob");
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::from(body)))
                    .expect("response")
            }
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .expect("response"),
        },
        "delete" => {
            bucket.remove(&key);
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Full::new(Bytes::new()))
                .expect("response")
        }
        "list" => {
            let names: Vec<String> = bucket.keys().cloned().collect();
            let body = serde_json::to_vec(&names).expect("serialize list");
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(body)))
                .expect("response")
        }
        _ => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from_static(b"unknown operation")))
            .expect("response"),
    }
}

#[tokio::test]
async fn state_get_set_delete() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    conformance::state_get_set_delete(&dapr).await;
}

#[tokio::test]
async fn state_ttl_expires() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    conformance::state_ttl_expires(&dapr).await;
}

#[tokio::test]
async fn state_cas() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    conformance::state_cas(&dapr).await;
}

#[tokio::test]
async fn state_increment() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    conformance::state_increment(&dapr).await;
}

#[tokio::test]
async fn publish_send() {
    let (sidecar, dapr) = FakeSidecar::serve().await;
    conformance::publish_send(&dapr).await;

    let published = sidecar.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].pubsub, "pubsub");
    assert_eq!(published[0].topic, "conformance.events");
    assert_eq!(published[0].query.as_deref(), Some("metadata.x-trace=1"));
    assert_eq!(published[0].payload, b"conformance-event");
}

#[tokio::test]
async fn config_get() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    conformance::config_get(&dapr).await;
}

#[tokio::test]
async fn blobstore_partial_bindings() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    let container = "dapr-blobs";

    assert_eq!(BlobStore::get(&dapr, container, "missing").await.expect("missing is None"), None);
    dapr.put(container, "digits", b"0123456789").await.expect("put digits");
    assert_eq!(
        BlobStore::get(&dapr, container, "digits").await.expect("get digits"),
        Some(b"0123456789".to_vec())
    );
    assert!(dapr.has(container, "digits").await.expect("has digits"));

    dapr.copy_object(container, "digits", container, "copy").await.expect("copy_object");
    dapr.move_object(container, "copy", container, "moved").await.expect("move_object");

    let mut names = dapr.list(container).await.expect("list");
    names.sort();
    assert_eq!(names, ["digits", "moved"]);

    BlobStore::delete(&dapr, container, "moved").await.expect("delete moved");
    assert!(!dapr.has(container, "moved").await.expect("moved gone"));

    // Documented contract deviations: range, metadata, and container ops return Unsupported.
    assert!(
        dapr.get_range(container, "digits", 0, 3)
            .await
            .expect_err("get_range unsupported")
            .to_string()
            .contains("unsupported")
    );
    assert!(
        dapr.object_info(container, "digits")
            .await
            .expect_err("object_info unsupported")
            .to_string()
            .contains("unsupported")
    );
    assert!(
        dapr.create_container(container)
            .await
            .expect_err("create_container unsupported")
            .to_string()
            .contains("unsupported")
    );
    assert!(
        dapr.delete_container(container)
            .await
            .expect_err("delete_container unsupported")
            .to_string()
            .contains("unsupported")
    );
    assert!(
        dapr.container_exists(container)
            .await
            .expect_err("container_exists unsupported")
            .to_string()
            .contains("unsupported")
    );
    assert!(
        dapr.container_info(container)
            .await
            .expect_err("container_info unsupported")
            .to_string()
            .contains("unsupported")
    );
}

#[derive(Clone, omnia_sdk::Provider)]
struct AppProvider {
    #[provider(StateStore)]
    dapr: Dapr<Direct<HttpDefault>>,
}

#[tokio::test]
async fn subscriptions_cloudevents_bridge() {
    let (_sidecar, dapr) = FakeSidecar::serve().await;
    let provider = AppProvider { dapr: dapr.clone() };

    let messaging = MessagingRouter::new(Client::new("dapr-app", provider.clone()))
        .route(
            "orders.created",
            consume(|order: String, cx: Context<AppProvider>| async move {
                cx.provider()
                    .set("orders/last", order.as_bytes(), None)
                    .await
                    .map_err(Error::from)?;
                Ok::<_, Error>(())
            }),
        )
        .route(
            "iot/sensors/temp",
            consume(|reading: String, cx: Context<AppProvider>| async move {
                let req_id = cx.metadata.request_id.as_deref().unwrap_or("");
                cx.provider()
                    .set("iot/last", format!("{reading}:{req_id}").as_bytes(), None)
                    .await
                    .map_err(Error::from)?;
                Ok::<_, Error>(())
            }),
        );

    let app = subscriptions(messaging, &dapr);

    // 1. GET /dapr/subscribe lists both topics in registration order.
    let sub_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dapr/subscribe")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("subscribe response");
    assert_eq!(sub_resp.status(), StatusCode::OK);
    let sub_bytes = sub_resp.into_body().collect().await.expect("body").to_bytes();
    let subs: Value = serde_json::from_slice(&sub_bytes).expect("json");
    assert_eq!(
        subs,
        json!([
            {"pubsubname": "pubsub", "topic": "iot/sensors/temp", "route": "/dapr/events/iot/sensors/temp"},
            {"pubsubname": "pubsub", "topic": "orders.created", "route": "/dapr/events/orders.created"}
        ])
    );

    // 2. Structured CloudEvents POST on /dapr/events/orders.created.
    let structured = json!({
        "specversion": "1.0",
        "id": "e-1",
        "source": "checkout",
        "type": "com.example.order",
        "datacontenttype": "application/json",
        "data": "ord-99"
    });
    let ev_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dapr/events/orders.created")
                .header(CONTENT_TYPE, "application/cloudevents+json")
                .body(axum::body::Body::from(structured.to_string()))
                .expect("request"),
        )
        .await
        .expect("event response");
    assert_eq!(ev_resp.status(), StatusCode::OK);
    let ack: Value = serde_json::from_slice(
        &ev_resp.into_body().collect().await.expect("body").to_bytes(),
    )
    .expect("json");
    assert_eq!(ack, json!({"status": "SUCCESS"}));
    assert_eq!(StateStore::get(&dapr, "orders/last").await.expect("get"), Some(b"ord-99".to_vec()));

    // 3. Binary CloudEvents POST on multi-segment topic /dapr/events/iot/sensors/temp.
    let mut bin_headers = HeaderMap::new();
    bin_headers.insert(CONTENT_TYPE, "application/json".parse().expect("header"));
    bin_headers.insert("ce-specversion", "1.0".parse().expect("header"));
    bin_headers.insert("ce-id", "e-2".parse().expect("header"));
    bin_headers.insert("ce-request-id", "req-42".parse().expect("header"));
    let mut bin_req = Request::builder()
        .method("POST")
        .uri("/dapr/events/iot/sensors/temp")
        .body(axum::body::Body::from(r#""21.5C""#))
        .expect("request");
    *bin_req.headers_mut() = bin_headers;

    let bin_resp = app.clone().oneshot(bin_req).await.expect("binary event response");
    let bin_ack: Value = serde_json::from_slice(
        &bin_resp.into_body().collect().await.expect("body").to_bytes(),
    )
    .expect("json");
    assert_eq!(bin_ack, json!({"status": "SUCCESS"}));
    assert_eq!(StateStore::get(&dapr, "iot/last").await.expect("get"), Some(b"21.5C:req-42".to_vec()));

    // 4. Unhandled topic answers DROP.
    let drop_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dapr/events/unrouted.topic")
                .header(CONTENT_TYPE, "application/cloudevents+json")
                .body(axum::body::Body::from(structured.to_string()))
                .expect("request"),
        )
        .await
        .expect("drop response");
    let drop_ack: Value = serde_json::from_slice(
        &drop_resp.into_body().collect().await.expect("body").to_bytes(),
    )
    .expect("json");
    assert_eq!(drop_ack, json!({"status": "DROP"}));
}

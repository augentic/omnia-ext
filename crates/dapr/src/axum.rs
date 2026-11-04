//! Axum router mounting Dapr's `/dapr/subscribe` and `/dapr/events/{*topic}` routes.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use http::HeaderMap;
use omnia_sdk::api::messaging::{DeliveryError, Router as MessagingRouter};
use serde::Serialize;

use crate::{Dapr, cloudevents};

#[derive(Clone, Debug, Serialize)]
struct Subscription {
    pubsubname: String,
    topic: String,
    route: String,
}

#[derive(Clone, Debug, Serialize)]
struct EventAck {
    status: &'static str,
}

struct BridgeState<P: Send + Sync + 'static> {
    router: MessagingRouter<P>,
    subscriptions: Vec<Subscription>,
}

/// Builds an [`axum::Router`] that exposes Dapr programmatic subscriptions for
/// `router` and dispatches inbound `CloudEvents` 1.0 deliveries into
/// [`MessagingRouter::handle`].
///
/// Mounts two routes:
///
/// - `GET /dapr/subscribe` returning the list of subscribed topics on
///   [`Dapr::pubsub`].
/// - `POST /dapr/events/{*topic}` decoding structured or binary `CloudEvents`
///   1.0 requests and returning `{"status": "SUCCESS" | "RETRY" | "DROP"}`.
pub fn subscriptions<P, H>(router: MessagingRouter<P>, dapr: &Dapr<H>) -> Router
where
    P: Send + Sync + 'static,
{
    let pubsub = dapr.pubsub().to_owned();
    let subscriptions = router
        .topics()
        .map(|topic| Subscription {
            pubsubname: pubsub.clone(),
            topic: topic.to_owned(),
            route: format!("/dapr/events/{topic}"),
        })
        .collect();

    let state = Arc::new(BridgeState {
        router,
        subscriptions,
    });

    Router::new()
        .route("/dapr/subscribe", get(list_subscriptions::<P>))
        .route("/dapr/events/{*topic}", post(deliver_event::<P>))
        .with_state(state)
}

async fn list_subscriptions<P: Send + Sync + 'static>(
    State(state): State<Arc<BridgeState<P>>>,
) -> Json<Vec<Subscription>> {
    Json(state.subscriptions.clone())
}

async fn deliver_event<P: Send + Sync + 'static>(
    State(state): State<Arc<BridgeState<P>>>, Path(topic): Path<String>, headers: HeaderMap,
    body: Bytes,
) -> Json<EventAck> {
    let topic = topic.trim_start_matches('/');
    let Ok(delivery) = cloudevents::decode(&headers, &body, Some(topic)) else {
        return Json(EventAck { status: "DROP" });
    };

    let status = match state.router.handle(delivery).await {
        Ok(()) => "SUCCESS",
        Err(DeliveryError::MissingTopic | DeliveryError::UnhandledTopic(_)) => "DROP",
        Err(DeliveryError::Rejected(_)) => "RETRY",
    };
    Json(EventAck { status })
}

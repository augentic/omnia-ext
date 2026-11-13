//! # HTTP Cache Wasm Guest
//!
//! Serves `GET /cached` by fetching `{UPSTREAM_URL}/resource` through
//! `HttpCache`, so repeated requests within `max-age` are answered from
//! `wasi:keyvalue` instead of the origin.

#![cfg(target_arch = "wasm32")]

use anyhow::Context as _;
use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use http::Method;
use http::header::{CACHE_CONTROL, IF_NONE_MATCH};
use omnia_http_cache::HttpCache;
use omnia_sdk::api::{Client, Context, Handlers, Router};
use omnia_sdk::{ConfigExt as _, HttpClient as _, WasiProvider};

omnia_sdk::api::http::export!(router);

fn router(provider: WasiProvider) -> Router<WasiProvider> {
    Handlers::new().route("/cached", get(cached)).with_state(Client::new(provider))
}

/// The first call reaches the origin; later calls within `max-age` are served
/// from the store. Either way the response carries `ETag: "v1"`.
#[omnia_wasi_otel::instrument]
async fn cached(context: Context<WasiProvider>) -> omnia_sdk::Result<impl IntoResponse> {
    let upstream: String = context.config().require("UPSTREAM_URL").await?;
    tracing::info!("fetching {upstream}/resource through the cache");

    let request = http::Request::builder()
        .method(Method::GET)
        .uri(format!("{upstream}/resource"))
        .header(CACHE_CONTROL, "max-age=300")
        .header(IF_NONE_MATCH, "\"v1\"")
        .body(Bytes::new())
        .context("building request")?;

    let provider = *context.provider();
    let response = HttpCache::new(provider, provider).fetch(request).await?;
    let (parts, body) = response.into_parts();

    Ok(http::Response::from_parts(parts, Body::from(body)))
}

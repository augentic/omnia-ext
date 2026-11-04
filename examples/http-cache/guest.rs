//! # HTTP Cache Wasm Guest
//!
//! Serves `GET /cached` by fetching `{UPSTREAM_URL}/resource` through
//! `HttpCache`, so repeated requests within `max-age` are answered from
//! `wasi:keyvalue` instead of the origin.

#![cfg(target_arch = "wasm32")]

use anyhow::Context;
use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use http::Method;
use http::header::{CACHE_CONTROL, IF_NONE_MATCH};
use http_body_util::Empty;
use omnia_http_cache::HttpCache;
use omnia_sdk::{Config, HttpRequest, HttpResult, WasiProvider};

omnia_sdk::http!(Router::new().route("/cached", get(cached)));

/// The first call reaches the origin; later calls within `max-age` are served
/// from the store. Either way the response carries `ETag: "v1"`.
#[omnia_wasi_otel::instrument]
async fn cached() -> HttpResult<impl IntoResponse> {
    let upstream = Config::get(&WasiProvider, "UPSTREAM_URL").await?;
    tracing::info!("fetching {upstream}/resource through the cache");

    let request = http::Request::builder()
        .method(Method::GET)
        .uri(format!("{upstream}/resource"))
        .header(CACHE_CONTROL, "max-age=300")
        .header(IF_NONE_MATCH, "\"v1\"")
        .body(Empty::<Bytes>::new())
        .context("building request")?;

    let response = HttpCache::new(WasiProvider, WasiProvider).fetch(request).await?;
    let (parts, body) = response.into_parts();

    Ok(http::Response::from_parts(parts, Body::from(body)))
}

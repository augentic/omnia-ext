#![doc = include_str!("../README.md")]

pub mod axum;
pub mod cloudevents;
mod protocol;

use std::collections::HashMap;
use std::future::{Future, ready};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use omnia_sdk::blobstore::{ContainerMetadata, ObjectMetadata};
use omnia_sdk::{BlobStore, CasError, Config, HttpRequest, Message, Publish, StateStore};

use crate::protocol::CasOutcome;

const DEFAULT_HTTP_PORT: &str = "3500";
const DEFAULT_STATE_STORE: &str = "statestore";
const DEFAULT_PUBSUB: &str = "pubsub";
const DEFAULT_CONFIG_STORE: &str = "configstore";
const MAX_INCREMENT_RETRIES: usize = 16;

/// Dapr HTTP v1.0 sidecar provider implementing [`StateStore`], [`Publish`],
/// [`Config`], and [`BlobStore`] over any [`HttpRequest`] transport.
#[derive(Clone, Debug)]
pub struct Dapr<H> {
    http: H,
    endpoint: String,
    state_store: String,
    pubsub: String,
    config_store: String,
    env_fallback: bool,
    ttls: Arc<Mutex<HashMap<String, u64>>>,
}

impl<H> Dapr<H> {
    /// Creates a [`Dapr`] provider over `http` with default sidecar settings
    /// (`http://127.0.0.1:3500`, `"statestore"`, `"pubsub"`, `"configstore"`).
    #[must_use]
    pub fn new(http: H) -> Self {
        Self {
            http,
            endpoint: format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}"),
            state_store: DEFAULT_STATE_STORE.to_owned(),
            pubsub: DEFAULT_PUBSUB.to_owned(),
            config_store: DEFAULT_CONFIG_STORE.to_owned(),
            env_fallback: false,
            ttls: Arc::default(),
        }
    }

    /// Configures a [`Dapr`] provider from process environment variables:
    /// `DAPR_HTTP_ENDPOINT` (or `http://127.0.0.1:$DAPR_HTTP_PORT`),
    /// `DAPR_STATE_STORE`, `DAPR_PUBSUB`, and `DAPR_CONFIG_STORE`.
    ///
    /// # Errors
    ///
    /// Returns an error if `DAPR_HTTP_ENDPOINT` is set to a non-HTTP(S) URL.
    pub fn from_env(http: H) -> Result<Self> {
        let port = std::env::var("DAPR_HTTP_PORT").unwrap_or_else(|_| DEFAULT_HTTP_PORT.to_owned());
        let endpoint = std::env::var("DAPR_HTTP_ENDPOINT")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{port}"));
        anyhow::ensure!(
            endpoint.starts_with("http://") || endpoint.starts_with("https://"),
            "DAPR_HTTP_ENDPOINT must be an http(s) URL, got `{endpoint}`"
        );
        let state_store =
            std::env::var("DAPR_STATE_STORE").unwrap_or_else(|_| DEFAULT_STATE_STORE.to_owned());
        let pubsub = std::env::var("DAPR_PUBSUB").unwrap_or_else(|_| DEFAULT_PUBSUB.to_owned());
        let config_store =
            std::env::var("DAPR_CONFIG_STORE").unwrap_or_else(|_| DEFAULT_CONFIG_STORE.to_owned());

        Ok(Self {
            http,
            endpoint,
            state_store,
            pubsub,
            config_store,
            env_fallback: false,
            ttls: Arc::default(),
        })
    }

    /// Overrides the Dapr HTTP sidecar base URL.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Overrides the Dapr state store component name.
    #[must_use]
    pub fn state_store(mut self, state_store: impl Into<String>) -> Self {
        self.state_store = state_store.into();
        self
    }

    /// Overrides the Dapr pub/sub component name.
    #[must_use]
    pub fn pubsub_name(mut self, pubsub: impl Into<String>) -> Self {
        self.pubsub = pubsub.into();
        self
    }

    /// Overrides the Dapr configuration store component name.
    #[must_use]
    pub fn config_store(mut self, config_store: impl Into<String>) -> Self {
        self.config_store = config_store.into();
        self
    }

    /// Enables or disables process-environment fallback before querying the
    /// Dapr configuration store.
    #[must_use]
    pub const fn env_fallback(mut self, enabled: bool) -> Self {
        self.env_fallback = enabled;
        self
    }

    /// The configured Dapr pub/sub component name.
    #[must_use]
    pub fn pubsub(&self) -> &str {
        &self.pubsub
    }

    fn record_ttl(&self, key: &str, ttl_secs: Option<u64>) {
        let mut ttls = self.ttls.lock().expect("ttl map lock");
        match ttl_secs {
            Some(secs) => {
                ttls.insert(key.to_owned(), secs);
            }
            None => {
                ttls.remove(key);
            }
        }
    }

    fn tracked_ttl_secs(&self, key: &str) -> Option<u64> {
        self.ttls.lock().expect("ttl map lock").get(key).copied()
    }
}

impl<H: HttpRequest> StateStore for Dapr<H> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let req = protocol::state_get_request(&self.endpoint, &self.state_store, key)?;
        let resp = self.http.fetch(req).await?;
        Ok(protocol::parse_state_get(&resp)?.map(|entry| entry.value))
    }

    async fn set(&self, key: &str, value: &[u8], ttl_secs: Option<u64>) -> Result<()> {
        let req =
            protocol::state_set_request(&self.endpoint, &self.state_store, key, value, ttl_secs)?;
        let resp = self.http.fetch(req).await?;
        protocol::ensure_success("state POST", &resp, false)?;
        self.record_ttl(key, ttl_secs);
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let req = protocol::state_delete_request(&self.endpoint, &self.state_store, key)?;
        let resp = self.http.fetch(req).await?;
        protocol::ensure_success("state DELETE", &resp, true)?;
        self.record_ttl(key, None);
        Ok(())
    }

    async fn cas(&self, key: &str, expected: Option<&[u8]>, value: &[u8]) -> Result<(), CasError> {
        // round trip 1: read current value and ETag
        let get_req = protocol::state_get_request(&self.endpoint, &self.state_store, key)
            .map_err(|e| CasError::Store(e.to_string()))?;
        let get_resp =
            self.http.fetch(get_req).await.map_err(|e| CasError::Store(e.to_string()))?;
        let current =
            protocol::parse_state_get(&get_resp).map_err(|e| CasError::Store(e.to_string()))?;

        let observed_bytes = current.as_ref().map(|e| e.value.as_slice());
        if observed_bytes != expected {
            return Err(CasError::Conflict(current.map(|e| e.value)));
        }

        // round trip 2: conditional first-write POST preserving TTL
        let etag = current.as_ref().and_then(|e| e.etag.as_deref());
        let ttl_secs = self.tracked_ttl_secs(key);
        let cas_req = protocol::state_cas_request(
            &self.endpoint,
            &self.state_store,
            key,
            value,
            etag,
            ttl_secs,
        )
        .map_err(|e| CasError::Store(e.to_string()))?;
        let cas_resp =
            self.http.fetch(cas_req).await.map_err(|e| CasError::Store(e.to_string()))?;

        match protocol::parse_state_cas(&cas_resp).map_err(|e| CasError::Store(e.to_string()))? {
            CasOutcome::Committed => Ok(()),
            CasOutcome::Conflict => {
                let reread_req =
                    protocol::state_get_request(&self.endpoint, &self.state_store, key)
                        .map_err(|e| CasError::Store(e.to_string()))?;
                let reread_resp = self
                    .http
                    .fetch(reread_req)
                    .await
                    .map_err(|e| CasError::Store(e.to_string()))?;
                let latest = protocol::parse_state_get(&reread_resp)
                    .map_err(|e| CasError::Store(e.to_string()))?;
                Err(CasError::Conflict(latest.map(|e| e.value)))
            }
        }
    }

    async fn increment(&self, key: &str, delta: i64) -> Result<i64> {
        for _ in 0..MAX_INCREMENT_RETRIES {
            let current = StateStore::get(self, key).await?;
            let base = match &current {
                None => 0,
                Some(bytes) => {
                    let arr: [u8; 8] = bytes.as_slice().try_into().map_err(|_len| {
                        anyhow!(
                            "value at `{key}` is {} bytes, not an 8-byte big-endian integer",
                            bytes.len()
                        )
                    })?;
                    i64::from_be_bytes(arr)
                }
            };
            let next = base
                .checked_add(delta)
                .with_context(|| format!("incrementing `{key}` by {delta} overflows i64"))?;
            match self.cas(key, current.as_deref(), &next.to_be_bytes()).await {
                Ok(()) => return Ok(next),
                Err(CasError::Conflict(_)) => {}
                Err(CasError::Store(err)) => bail!("incrementing `{key}` failed: {err}"),
            }
        }
        bail!("incrementing `{key}` exceeded {MAX_INCREMENT_RETRIES} CAS retries under contention")
    }
}

impl<H: HttpRequest> Publish for Dapr<H> {
    async fn send(&self, topic: &str, message: &Message) -> Result<()> {
        let req = protocol::publish_request(
            &self.endpoint,
            &self.pubsub,
            topic,
            &message.payload,
            &message.headers,
        )?;
        let resp = self.http.fetch(req).await?;
        protocol::ensure_success("publish POST", &resp, false)
    }
}

impl<H: HttpRequest> Config for Dapr<H> {
    async fn get(&self, key: &str) -> Result<String> {
        if self.env_fallback
            && let Ok(val) = std::env::var(key)
        {
            return Ok(val);
        }
        let req = protocol::config_get_request(&self.endpoint, &self.config_store, key)?;
        let resp = self.http.fetch(req).await?;
        protocol::parse_config_get(&resp, key)
    }
}

impl<H: HttpRequest> BlobStore for Dapr<H> {
    async fn get(&self, container: &str, name: &str) -> Result<Option<Vec<u8>>> {
        let req = protocol::binding_request(&self.endpoint, container, "get", Some(name), None)?;
        let resp = self.http.fetch(req).await?;
        protocol::parse_binding_get(&resp)
    }

    async fn put(&self, container: &str, name: &str, data: &[u8]) -> Result<()> {
        let req =
            protocol::binding_request(&self.endpoint, container, "create", Some(name), Some(data))?;
        let resp = self.http.fetch(req).await?;
        protocol::ensure_success("binding create", &resp, false)
    }

    async fn delete(&self, container: &str, name: &str) -> Result<()> {
        let req = protocol::binding_request(&self.endpoint, container, "delete", Some(name), None)?;
        let resp = self.http.fetch(req).await?;
        protocol::ensure_success("binding delete", &resp, true)
    }

    async fn list(&self, container: &str) -> Result<Vec<String>> {
        let req = protocol::binding_request(&self.endpoint, container, "list", None, None)?;
        let resp = self.http.fetch(req).await?;
        protocol::parse_binding_list(&resp)
    }

    fn get_range(
        &self, _container: &str, _name: &str, _start: u64, _end: u64,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: get_range")))
    }

    fn object_info(
        &self, _container: &str, _name: &str,
    ) -> impl Future<Output = Result<ObjectMetadata>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: object_info")))
    }

    fn create_container(&self, _name: &str) -> impl Future<Output = Result<()>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: create_container")))
    }

    fn delete_container(&self, _name: &str) -> impl Future<Output = Result<()>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: delete_container")))
    }

    fn container_exists(&self, _name: &str) -> impl Future<Output = Result<bool>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: container_exists")))
    }

    fn container_info(
        &self, _name: &str,
    ) -> impl Future<Output = Result<ContainerMetadata>> + Send {
        ready(Err(anyhow!("unsupported by Dapr output bindings: container_info")))
    }
}

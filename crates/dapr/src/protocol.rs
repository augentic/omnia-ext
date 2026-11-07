//! Pure HTTP request builders and response parsers for the Dapr v1.0 sidecar API.

use std::fmt::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};
use base64ct::{Base64, Encoding as _};
use bytes::Bytes;
use http::header::{CONTENT_TYPE, ETAG};
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateEntry {
    pub value: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Serialize)]
struct StateWriteItem<'a> {
    key: &'a str,
    value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    etag: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<StateMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<StateOptions>,
}

#[derive(Serialize)]
struct StateMetadata {
    #[serde(rename = "ttlInSeconds")]
    ttl_in_seconds: String,
}

#[derive(Serialize)]
struct StateOptions {
    concurrency: &'static str,
}

#[derive(Serialize)]
struct BindingRequest<'a> {
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<BindingMetadata<'a>>,
}

#[derive(Serialize)]
struct BindingMetadata<'a> {
    key: &'a str,
    #[serde(rename = "blobName")]
    blob_name: &'a str,
}

#[derive(Deserialize)]
struct ConfigItem {
    value: Option<String>,
}

// --- State API ---

pub fn state_get_request(endpoint: &str, store: &str, key: &str) -> Result<Request<Full<Bytes>>> {
    let url = format!("{}/v1.0/state/{}/{}", trim_slash(endpoint), encode(store), encode(key));
    Request::builder()
        .method(Method::GET)
        .uri(url)
        .body(Full::new(Bytes::new()))
        .context("building Dapr state GET request")
}

pub fn parse_state_get(response: &Response<Bytes>) -> Result<Option<StateEntry>> {
    match response.status() {
        StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
        status if status.is_success() => {
            let body = response.body();
            if body.is_empty() {
                return Ok(None);
            }
            let value = decode_bytes(body)?;
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim_matches('"').to_owned())
                .filter(|s| !s.is_empty());
            Ok(Some(StateEntry { value, etag }))
        }
        status => {
            bail!("Dapr state GET failed ({status}): {}", String::from_utf8_lossy(response.body()))
        }
    }
}

pub fn state_set_request(
    endpoint: &str, store: &str, key: &str, value: &[u8], ttl_secs: Option<u64>,
) -> Result<Request<Full<Bytes>>> {
    let url = format!("{}/v1.0/state/{}", trim_slash(endpoint), encode(store));
    let item = StateWriteItem {
        key,
        value: encode_bytes(value),
        etag: None,
        metadata: ttl_secs.map(|secs| StateMetadata {
            ttl_in_seconds: secs.to_string(),
        }),
        options: None,
    };
    let body = serde_json::to_vec(&[item]).context("serializing Dapr state write")?;
    Request::builder()
        .method(Method::POST)
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .context("building Dapr state POST request")
}

pub fn state_cas_request(
    endpoint: &str, store: &str, key: &str, value: &[u8], etag: Option<&str>, ttl_secs: Option<u64>,
) -> Result<Request<Full<Bytes>>> {
    let url = format!("{}/v1.0/state/{}", trim_slash(endpoint), encode(store));
    let item = StateWriteItem {
        key,
        value: encode_bytes(value),
        etag: Some(etag.unwrap_or("")),
        metadata: ttl_secs.map(|secs| StateMetadata {
            ttl_in_seconds: secs.to_string(),
        }),
        options: Some(StateOptions {
            concurrency: "first-write",
        }),
    };
    let body = serde_json::to_vec(&[item]).context("serializing Dapr state CAS write")?;
    Request::builder()
        .method(Method::POST)
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .context("building Dapr state CAS POST request")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CasOutcome {
    Committed,
    Conflict,
}

pub fn parse_state_cas(response: &Response<Bytes>) -> Result<CasOutcome> {
    match response.status() {
        status if status.is_success() => Ok(CasOutcome::Committed),
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => Ok(CasOutcome::Conflict),
        status => {
            bail!("Dapr state CAS failed ({status}): {}", String::from_utf8_lossy(response.body()))
        }
    }
}

pub fn state_delete_request(
    endpoint: &str, store: &str, key: &str,
) -> Result<Request<Full<Bytes>>> {
    let url = format!("{}/v1.0/state/{}/{}", trim_slash(endpoint), encode(store), encode(key));
    Request::builder()
        .method(Method::DELETE)
        .uri(url)
        .body(Full::new(Bytes::new()))
        .context("building Dapr state DELETE request")
}

pub fn ensure_success(op: &str, response: &Response<Bytes>, allow_not_found: bool) -> Result<()> {
    let status = response.status();
    if status.is_success() || (allow_not_found && status == StatusCode::NOT_FOUND) {
        return Ok(());
    }
    bail!("Dapr {op} failed ({status}): {}", String::from_utf8_lossy(response.body()))
}

// --- Pub/Sub API ---

pub fn publish_request<'a>(
    endpoint: &str, pubsub: &str, topic: &str, payload: &[u8],
    headers: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> Result<Request<Full<Bytes>>> {
    let mut pairs: Vec<(&str, &str)> =
        headers.into_iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    pairs.sort_unstable_by_key(|(k, _)| *k);

    let mut url =
        format!("{}/v1.0/publish/{}/{}", trim_slash(endpoint), encode(pubsub), encode_topic(topic));
    if !pairs.is_empty() {
        let query: Vec<String> = pairs
            .into_iter()
            .map(|(k, v)| format!("metadata.{}={}", encode(k), encode(v)))
            .collect();
        url.push('?');
        url.push_str(&query.join("&"));
    }

    Request::builder()
        .method(Method::POST)
        .uri(url)
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(Full::new(Bytes::copy_from_slice(payload)))
        .context("building Dapr publish request")
}

// --- Configuration API ---

pub fn config_get_request(endpoint: &str, store: &str, key: &str) -> Result<Request<Full<Bytes>>> {
    let url = format!(
        "{}/v1.0/configuration/{}?key={}",
        trim_slash(endpoint),
        encode(store),
        encode(key)
    );
    Request::builder()
        .method(Method::GET)
        .uri(url)
        .body(Full::new(Bytes::new()))
        .context("building Dapr configuration GET request")
}

pub fn parse_config_get(response: &Response<Bytes>, key: &str) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        bail!(
            "Dapr configuration GET for `{key}` failed ({status}): {}",
            String::from_utf8_lossy(response.body())
        );
    }
    let body: Value =
        serde_json::from_slice(response.body()).context("parsing Dapr configuration JSON")?;

    // Map shape: `{"KEY": {"value": "val"}}`
    if let Some(obj) = body.as_object() {
        if let Some(entry) = obj.get(key) {
            let item: ConfigItem =
                serde_json::from_value(entry.clone()).context("parsing configuration item")?;
            if let Some(val) = item.value {
                return Ok(val);
            }
        }
        bail!("configuration key `{key}` absent in Dapr response");
    }

    // Array shape: `[{"key": "KEY", "value": "val"}]`
    if let Some(arr) = body.as_array() {
        for entry in arr {
            if entry.get("key").and_then(Value::as_str) == Some(key)
                && let Some(val) = entry.get("value").and_then(Value::as_str)
            {
                return Ok(val.to_owned());
            }
        }
        bail!("configuration key `{key}` absent in Dapr response");
    }

    bail!("unexpected Dapr configuration response shape")
}

// --- Output Bindings (partial BlobStore) ---

pub fn binding_request(
    endpoint: &str, container: &str, operation: &str, key: Option<&str>, data: Option<&[u8]>,
) -> Result<Request<Full<Bytes>>> {
    let url = format!("{}/v1.0/bindings/{}", trim_slash(endpoint), encode(container));
    let payload = BindingRequest {
        operation,
        data: data.map(encode_bytes),
        metadata: key.map(|k| BindingMetadata { key: k, blob_name: k }),
    };
    let body = serde_json::to_vec(&payload).context("serializing Dapr binding request")?;
    Request::builder()
        .method(Method::POST)
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .context("building Dapr binding POST request")
}

pub fn parse_binding_get(response: &Response<Bytes>) -> Result<Option<Vec<u8>>> {
    match response.status() {
        StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
        status if status.is_success() => {
            let body = response.body();
            if body.is_empty() {
                return Ok(None);
            }
            decode_bytes(body).map(Some)
        }
        status => bail!(
            "Dapr binding get failed ({status}): {}",
            String::from_utf8_lossy(response.body())
        ),
    }
}

pub fn parse_binding_list(response: &Response<Bytes>) -> Result<Vec<String>> {
    let status = response.status();
    if !status.is_success() {
        bail!("Dapr binding list failed ({status}): {}", String::from_utf8_lossy(response.body()));
    }
    let value: Value =
        serde_json::from_slice(response.body()).context("parsing Dapr binding list JSON")?;
    let Some(items) = value.as_array() else {
        bail!("expected JSON array from Dapr binding list");
    };
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        if let Some(name) = item.as_str() {
            names.push(name.to_owned());
        } else if let Some(name) =
            item.get("name").or_else(|| item.get("key")).and_then(Value::as_str)
        {
            names.push(name.to_owned());
        } else {
            return Err(anyhow!("unrecognised item in Dapr binding list: {item}"));
        }
    }
    Ok(names)
}

// --- Byte-transparent encoding and URL helpers ---

pub fn encode_bytes(bytes: &[u8]) -> String {
    Base64::encode_string(bytes)
}

pub fn decode_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    if let Ok(encoded) = serde_json::from_slice::<String>(raw) {
        return Base64::decode_vec(&encoded).context("decoding base64 state payload");
    }
    if let Ok(text) = std::str::from_utf8(raw)
        && let Ok(decoded) = Base64::decode_vec(text.trim())
    {
        return Ok(decoded);
    }
    Ok(raw.to_vec())
}

fn trim_slash(endpoint: &str) -> &str {
    endpoint.trim_end_matches('/')
}

fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn encode_topic(topic: &str) -> String {
    topic.split('/').map(encode).collect::<Vec<_>>().join("/")
}

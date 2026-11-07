//! `CloudEvents` 1.0 HTTP request decoder (structured and binary content modes).

use base64ct::{Base64, Encoding as _};
use http::HeaderMap;
use http::header::CONTENT_TYPE;
use omnia_sdk::api::messaging::Delivery;
use serde_json::Value;

/// Errors returned when decoding an inbound `CloudEvents` 1.0 HTTP request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Structured `CloudEvents` body is not valid JSON or not a JSON object.
    #[error("invalid structured CloudEvents JSON: {0}")]
    InvalidJson(String),
    /// `data_base64` field is not valid base64.
    #[error("invalid CloudEvents data_base64: {0}")]
    InvalidBase64(String),
}

/// Decodes a structured (`application/cloudevents+json`) or binary (`ce-*`
/// headers) `CloudEvents` 1.0 HTTP request into a [`Delivery`].
///
/// When `route_topic` is supplied (for example, extracted from the
/// `/dapr/events/{*topic}` path parameter), it takes precedence over the
/// envelope's `topic` attribute so the router dispatches on the subscribed
/// route topic.
///
/// # Errors
///
/// Returns [`DecodeError`] if a structured `CloudEvents` body is malformed JSON
/// or contains invalid `data_base64`.
pub fn decode(
    headers: &HeaderMap, body: &[u8], route_topic: Option<&str>,
) -> Result<Delivery, DecodeError> {
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();

    if is_structured(content_type) {
        decode_structured(body, route_topic)
    } else {
        Ok(decode_binary(headers, body, route_topic, content_type))
    }
}

fn is_structured(content_type: &str) -> bool {
    let media = content_type.split(';').next().unwrap_or_default().trim();
    media.eq_ignore_ascii_case("application/cloudevents+json")
}

fn decode_structured(body: &[u8], route_topic: Option<&str>) -> Result<Delivery, DecodeError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|e| DecodeError::InvalidJson(e.to_string()))?;
    let Some(obj) = value.as_object() else {
        return Err(DecodeError::InvalidJson("expected JSON object".to_owned()));
    };

    let content_type = obj.get("datacontenttype").and_then(Value::as_str).map(str::to_owned);
    let is_json_data = content_type.as_deref().is_none_or(|ct| ct.contains("json"));

    let payload = if let Some(b64) = obj.get("data_base64").and_then(Value::as_str) {
        Base64::decode_vec(b64).map_err(|e| DecodeError::InvalidBase64(e.to_string()))?
    } else if let Some(data) = obj.get("data") {
        match data {
            Value::Null => Vec::new(),
            Value::String(s) if !is_json_data => s.as_bytes().to_vec(),
            other => {
                serde_json::to_vec(other).map_err(|e| DecodeError::InvalidJson(e.to_string()))?
            }
        }
    } else {
        Vec::new()
    };
    let topic = route_topic
        .map(str::to_owned)
        .or_else(|| obj.get("topic").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| obj.get("subject").and_then(Value::as_str).map(str::to_owned));

    let mut metadata = Vec::new();
    for (key, val) in obj {
        if matches!(key.as_str(), "data" | "data_base64" | "datacontenttype") {
            continue;
        }
        match val {
            Value::String(s) => metadata.push((key.clone(), s.clone())),
            Value::Number(n) => metadata.push((key.clone(), n.to_string())),
            Value::Bool(b) => metadata.push((key.clone(), b.to_string())),
            _ => {}
        }
    }

    Ok(Delivery {
        topic,
        payload,
        content_type,
        metadata,
    })
}

fn decode_binary(
    headers: &HeaderMap, body: &[u8], route_topic: Option<&str>, content_type: &str,
) -> Delivery {
    let mut metadata = Vec::new();
    let mut header_topic = None;

    for (name, value) in headers {
        let Ok(val) = value.to_str() else {
            continue;
        };
        if let Some(attr) = name.as_str().strip_prefix("ce-") {
            if matches!(attr, "topic" | "subject") && header_topic.is_none() {
                header_topic = Some(val.to_owned());
            }
            metadata.push((attr.to_owned(), val.to_owned()));
        }
    }

    let topic = route_topic.map(str::to_owned).or(header_topic);
    let content_type = (!content_type.is_empty()).then(|| content_type.to_owned());

    Delivery {
        topic,
        payload: body.to_vec(),
        content_type,
        metadata,
    }
}

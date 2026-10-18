use std::{fmt, time::Duration};

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::retry::parse_retry_after;

/// Result alias for SDK calls.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure the SDK reports.
#[derive(Debug)]
pub enum Error {
    /// Invalid input or configuration, found before or after a request (TS `TypeSafeError`).
    Sdk(String),
    /// The server returned a non-2xx response after retries.
    Api(ApiError),
    /// The request or response body failed to travel (DNS, TLS, connection closed, ...).
    Connection(String),
    /// The full response did not arrive within the per-attempt timeout.
    Timeout(Duration),
}

impl Error {
    pub(crate) fn sdk(message: impl Into<String>) -> Self {
        Self::Sdk(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sdk(m) | Self::Connection(m) => f.write_str(m),
            Self::Api(e) => e.fmt(f),
            Self::Timeout(d) => write!(f, "Request timed out after {}ms.", d.as_millis()),
        }
    }
}

impl std::error::Error for Error {}

/// The HTTP status classes the SDK names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiErrorKind {
    BadRequest,
    Authentication,
    PermissionDenied,
    NotFound,
    UnprocessableEntity,
    RateLimit,
    InternalServer,
    Other,
}

/// An unsuccessful HTTP response from the API.
#[derive(Debug)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub status: u16,
    pub headers: HeaderMap,
    /// Parsed JSON, response text as a string, or `None` for an empty body.
    pub body: Option<Value>,
    /// Value of `x-typesafe-request-id`, when present.
    pub request_id: Option<String>,
    message: String,
}

const MAX_RAW_BODY_IN_MESSAGE: usize = 200;

impl ApiError {
    pub(crate) fn from_response(status: u16, body: Option<Value>, headers: HeaderMap) -> Self {
        let kind = match status {
            400 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Authentication,
            403 => ApiErrorKind::PermissionDenied,
            404 => ApiErrorKind::NotFound,
            422 => ApiErrorKind::UnprocessableEntity,
            429 => ApiErrorKind::RateLimit,
            s if s >= 500 => ApiErrorKind::InternalServer,
            _ => ApiErrorKind::Other,
        };
        let request_id = request_id_from(&headers);
        let message = describe(status, body.as_ref());
        Self {
            kind,
            status,
            headers,
            body,
            request_id,
            message,
        }
    }

    /// Server retry delay from `retry-after-ms` or `Retry-After`, when valid.
    pub fn retry_after(&self) -> Option<Duration> {
        parse_retry_after(&self.headers)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub(crate) fn request_id_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-typesafe-request-id")?
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn describe(status: u16, body: Option<&Value>) -> String {
    if let Some(detail) = body.and_then(extract_message) {
        return format!("{status} {detail}");
    }
    let Some(body) = body else {
        return format!("{status} status code (no body)");
    };
    let raw = match body {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if raw.chars().count() > MAX_RAW_BODY_IN_MESSAGE {
        let cut: String = raw.chars().take(MAX_RAW_BODY_IN_MESSAGE).collect();
        format!("{status} {cut}…")
    } else {
        format!("{status} {raw}")
    }
}

/// Extract a message from a text, error, or validation response body.
fn extract_message(body: &Value) -> Option<String> {
    let obj = match body {
        Value::String(s) => return (!s.is_empty()).then(|| s.clone()),
        Value::Object(o) => o,
        _ => return None,
    };
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let nested = |v: &Value| v.get("message").and_then(text);
    if let Some(m) = obj.get("error").and_then(|e| text(e).or_else(|| nested(e))) {
        return Some(m);
    }
    if let Some(m) = obj.get("message").and_then(text) {
        return Some(m);
    }
    match obj.get("detail")? {
        Value::Array(items) => describe_validation_errors(items),
        d => text(d).or_else(|| nested(d)),
    }
}

/// Format validation errors as semicolon-separated `path: message` entries.
fn describe_validation_errors(errors: &[Value]) -> Option<String> {
    let parts: Vec<String> = errors
        .iter()
        .filter_map(|e| {
            let msg = e.get("msg")?.as_str()?;
            let loc = e
                .get("loc")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter(|x| x.as_str() != Some("body"))
                        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();
            Some(if loc.is_empty() {
                msg.to_owned()
            } else {
                format!("{loc}: {msg}")
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn msg(status: u16, body: Option<Value>) -> String {
        ApiError::from_response(status, body, HeaderMap::new()).to_string()
    }

    #[test]
    fn extracts_known_shapes() {
        assert_eq!(msg(400, Some(json!({"error": "bad"}))), "400 bad");
        assert_eq!(
            msg(400, Some(json!({"error": {"message": "nested"}}))),
            "400 nested"
        );
        assert_eq!(msg(500, Some(json!({"detail": "boom"}))), "500 boom");
        assert_eq!(
            msg(
                422,
                Some(json!({"detail": [{"loc": ["body", "state"], "msg": "required"}]}))
            ),
            "422 state: required"
        );
    }

    #[test]
    fn falls_back_to_raw_or_empty() {
        assert_eq!(msg(502, None), "502 status code (no body)");
        assert_eq!(msg(502, Some(json!("gateway"))), "502 gateway");
        assert!(msg(502, Some(json!({"x": "y".repeat(300)}))).ends_with('…'));
    }

    #[test]
    fn maps_status_to_kind() {
        let kind = |s| ApiError::from_response(s, None, HeaderMap::new()).kind;
        assert_eq!(kind(401), ApiErrorKind::Authentication);
        assert_eq!(kind(429), ApiErrorKind::RateLimit);
        assert_eq!(kind(503), ApiErrorKind::InternalServer);
        assert_eq!(kind(418), ApiErrorKind::Other);
    }
}

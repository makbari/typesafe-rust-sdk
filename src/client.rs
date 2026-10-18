use std::{fmt, time::Duration};

use reqwest::{
    Method,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    assets::{Body, encode_request},
    error::{ApiError, Error, Result},
    retry::{DEFAULT_TIMEOUT, RetryPolicy},
    types::{ModelCard, SystemOneRequest, SystemOneResult, validate_questions},
};

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";

const ENV_API_KEY: &str = "TYPESAFE_API_KEY";
const ENV_BASE_URL: &str = "TYPESAFE_BASE_URL";
const ENV_DEFAULT_MODEL: &str = "TYPESAFE_DEFAULT_MODEL";

/// Bodies below this many bytes fit in a single TCP segment, so they are sent uncompressed.
const MIN_COMPRESSED_BODY_BYTES: usize = 1024;

/// Request body compression, sent as `Content-Encoding`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// zstd for bodies of 1 KiB or more.
    #[default]
    Zstd,
    /// Send every body uncompressed.
    None,
}

/// Per-call options that override client settings.
#[derive(Clone, Debug, Default)]
pub struct RequestOptions {
    /// Timeout per attempt; there is no total retry budget.
    pub timeout: Option<Duration>,
    /// Replaces the client's retry policy for this call.
    // ponytail: whole-policy replace, not field-wise merge like the TS SDK.
    pub retry: Option<RetryPolicy>,
    headers: Vec<(String, String)>,
}

impl RequestOptions {
    /// Add a header, merged over the client's default headers.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Builder for [`Client`]. Explicit values win over environment variables, then defaults.
#[derive(Clone, Debug, Default)]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    default_model: Option<String>,
    timeout: Option<Duration>,
    retry: Option<RetryPolicy>,
    default_headers: Vec<(String, String)>,
    compression: Compression,
    http: Option<reqwest::Client>,
}

impl ClientBuilder {
    /// Falls back to `TYPESAFE_API_KEY`.
    pub fn api_key(mut self, v: impl Into<String>) -> Self {
        self.api_key = Some(v.into());
        self
    }
    /// Falls back to `TYPESAFE_BASE_URL`, then `https://api.typesafe.ai`.
    pub fn base_url(mut self, v: impl Into<String>) -> Self {
        self.base_url = Some(v.into());
        self
    }
    /// Falls back to `TYPESAFE_DEFAULT_MODEL`, then `jev-latest`.
    pub fn default_model(mut self, v: impl Into<String>) -> Self {
        self.default_model = Some(v.into());
        self
    }
    /// Timeout per attempt. Default: 10 s.
    pub fn timeout(mut self, v: Duration) -> Self {
        self.timeout = Some(v);
        self
    }
    pub fn retry(mut self, v: RetryPolicy) -> Self {
        self.retry = Some(v);
        self
    }
    /// Additional header on every request; per-call headers take precedence.
    pub fn default_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_headers.push((name.into(), value.into()));
        self
    }
    pub fn compression(mut self, v: Compression) -> Self {
        self.compression = v;
        self
    }
    /// Use a configured `reqwest::Client` (proxy, TLS roots, tests).
    pub fn http_client(mut self, v: reqwest::Client) -> Self {
        self.http = Some(v);
        self
    }

    /// # Errors
    /// The API key is missing or a setting is invalid.
    pub fn build(self) -> Result<Client> {
        let api_key = self.api_key.or_else(|| read_env(ENV_API_KEY)).ok_or_else(|| {
            Error::sdk(format!(
                "No API key was provided. Call `ClientBuilder::api_key` or set the {ENV_API_KEY} environment variable."
            ))
        })?;
        let base_url = self
            .base_url
            .or_else(|| read_env(ENV_BASE_URL))
            .unwrap_or_else(|| DEFAULT_BASE_URL.into())
            .trim_end_matches('/')
            .to_owned();
        let default_model = self
            .default_model
            .or_else(|| read_env(ENV_DEFAULT_MODEL))
            .unwrap_or_else(|| DEFAULT_MODEL.into());
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        check_timeout(timeout)?;
        let retry = self.retry.unwrap_or_default();
        retry.validate()?;
        let mut default_headers = HeaderMap::new();
        merge_headers(&mut default_headers, &self.default_headers)?;
        Ok(Client {
            http: self.http.unwrap_or_default(),
            api_key,
            base_url,
            default_model,
            timeout,
            retry,
            default_headers,
            compression: self.compression,
        })
    }
}

/// Client for the TypeSafe AI API. Cheap to clone; clones share one connection pool.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    timeout: Duration,
    retry: RetryPolicy,
    default_headers: HeaderMap,
    compression: Compression,
}

// Hand-written so the API key never reaches logs.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .field("compression", &self.compression)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Create a client from environment variables only.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Answer named questions about text, structured state, or images.
    pub async fn system_one(&self, request: SystemOneRequest) -> Result<SystemOneResult> {
        self.system_one_with(request, &RequestOptions::default())
            .await
    }

    /// Like [`system_one`](Self::system_one) with per-call options. Drop the future to cancel.
    pub async fn system_one_with(
        &self,
        request: SystemOneRequest,
        options: &RequestOptions,
    ) -> Result<SystemOneResult> {
        validate_questions(&request.questions)?;
        let payload = serde_json::json!({
            "state": request.state,
            "questions": request.questions,
            "model": request.model.unwrap_or_else(|| self.default_model.clone()),
        });
        let Value::Object(payload) = payload else {
            unreachable!("json! object literal")
        };
        let body = encode_request(payload, &request.assets, &request.images)?;
        self.send(Method::POST, "/v1/systemone", Some(body), options)
            .await
    }

    /// List the models available to the account.
    pub async fn list_models(&self) -> Result<Vec<ModelCard>> {
        self.list_models_with(&RequestOptions::default()).await
    }

    pub async fn list_models_with(&self, options: &RequestOptions) -> Result<Vec<ModelCard>> {
        #[derive(serde::Deserialize)]
        struct Wire {
            models: Vec<ModelCard>,
        }
        let wire: Wire = self.send(Method::GET, "/v1/models", None, options).await?;
        Ok(wire.models)
    }

    /// Send a request with retries and parse the 2xx body as `T`.
    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Body>,
        options: &RequestOptions,
    ) -> Result<T> {
        let timeout = options.timeout.unwrap_or(self.timeout);
        check_timeout(timeout)?;
        let retry = match &options.retry {
            Some(r) => {
                r.validate()?;
                r
            }
            None => &self.retry,
        };
        let url = format!("{}{path}", self.base_url);

        // Compressed once and sent as-is on every attempt.
        let (content_type, bytes, encoded) = match body {
            None => (None, None, false),
            Some(Body {
                content_type,
                bytes,
            }) if self.compression == Compression::Zstd
                && bytes.len() >= MIN_COMPRESSED_BODY_BYTES =>
            {
                let packed = zstd::bulk::compress(&bytes, 0)
                    .map_err(|e| Error::sdk(format!("zstd compression failed: {e}")))?;
                (Some(content_type), Some(packed), true)
            }
            Some(Body {
                content_type,
                bytes,
            }) => (Some(content_type), Some(bytes), false),
        };

        // User headers go first so they cannot clobber auth, content type, or encoding.
        let mut headers = self.default_headers.clone();
        merge_headers(&mut headers, &options.headers)?;
        headers.remove("x-typesafe-retry-count");
        let sdk_ua = HeaderValue::from_static(concat!("typesafe-sdk/", env!("CARGO_PKG_VERSION")));
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|_| Error::sdk("The API key contains characters not allowed in a header."))?;
        auth.set_sensitive(true);
        headers.insert("authorization", auth);
        headers.insert("accept", HeaderValue::from_static("application/json"));
        headers.insert("user-agent", sdk_ua.clone());
        headers.insert("x-typesafe-sdk", sdk_ua);
        headers.insert("x-typesafe-runtime", HeaderValue::from_static("rust"));
        headers.remove("content-type");
        headers.remove("content-encoding");
        if let Some(ct) = &content_type {
            headers.insert(
                "content-type",
                HeaderValue::from_str(ct).map_err(|_| Error::sdk("Invalid content type."))?,
            );
        }
        if encoded {
            headers.insert("content-encoding", HeaderValue::from_static("zstd"));
        }

        let mut attempt = 0u32;
        loop {
            let retries_left = retry.max_retries.saturating_sub(attempt);
            let mut attempt_headers = headers.clone();
            if attempt > 0 {
                attempt_headers.insert("x-typesafe-retry-count", HeaderValue::from(attempt));
            }
            let mut req = self
                .http
                .request(method.clone(), &url)
                .headers(attempt_headers)
                .timeout(timeout);
            if let Some(b) = &bytes {
                req = req.body(b.clone());
            }

            let wait = match attempt_once(req, timeout).await {
                Err(e) => {
                    if retries_left == 0 || !retry.retries_error(&e) {
                        return Err(e);
                    }
                    retry.delay(attempt, None)
                }
                Ok((status, _, text)) if (200..300).contains(&status) => {
                    return serde_json::from_str(&text)
                        .map_err(|e| Error::sdk(format!("Unexpected response from {path}: {e}")));
                }
                Ok((status, resp_headers, text)) => {
                    let error = ApiError::from_response(status, parse_body(&text), resp_headers);
                    if retries_left == 0 || !retry.http_statuses.contains(&status) {
                        return Err(Error::Api(error));
                    }
                    retry.delay(attempt, Some(&error.headers))
                }
            };
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }
}

/// One HTTP round trip, including body delivery, bounded by the reqwest timeout.
async fn attempt_once(
    req: reqwest::RequestBuilder,
    timeout: Duration,
) -> Result<(u16, HeaderMap, String)> {
    let map = |e: reqwest::Error| {
        if e.is_timeout() {
            Error::Timeout(timeout)
        } else {
            Error::Connection(format!("Connection error: {e}"))
        }
    };
    let res = req.send().await.map_err(map)?;
    let (status, headers) = (res.status().as_u16(), res.headers().clone());
    let text = res.text().await.map_err(map)?;
    Ok((status, headers, text))
}

/// Lenient: servers and proxies do not always set content-type, so try JSON, else keep text.
fn parse_body(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())))
}

fn read_env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_owned())
}

fn check_timeout(t: Duration) -> Result<()> {
    if t.is_zero() {
        return Err(Error::sdk("`timeout` must be a positive duration."));
    }
    Ok(())
}

/// Insert headers; a repeated name (any casing) replaces the earlier value.
fn merge_headers(into: &mut HeaderMap, pairs: &[(String, String)]) -> Result<()> {
    for (name, value) in pairs {
        let n = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::sdk(format!("Invalid header name \"{name}\".")))?;
        let v = HeaderValue::from_str(value)
            .map_err(|_| Error::sdk(format!("Invalid value for header \"{name}\".")))?;
        into.insert(n, v);
    }
    Ok(())
}

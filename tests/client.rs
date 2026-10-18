//! End-to-end tests against a tiny local HTTP server that replays canned responses.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use typesafe_sdk::{ApiErrorKind, Client, Error, Image, RetryPolicy, SystemOneRequest, noul};

const OK_BODY: &str = r#"{"model":"jev-latest","answers":{"billing":{"type":"noul","noul":0.97}},"usage":{"input_tokens":12,"output_tokens":3}}"#;

/// Serve `responses` (status, extra headers, body) in order, one per connection.
/// Returns the base URL and the raw requests seen (headers + body, lossy text).
async fn serve(
    responses: Vec<(u16, &'static str, &'static str)>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        for (status, headers, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                } else if n == 0 {
                    break;
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).to_string());
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (url, seen)
}

fn client(url: &str) -> Client {
    Client::builder()
        .api_key("sk-test-1234567890")
        .base_url(url)
        .retry(RetryPolicy {
            backoff_initial: Duration::from_millis(5),
            backoff_max: Duration::from_millis(10),
            ..Default::default()
        })
        .build()
        .unwrap()
}

fn request() -> SystemOneRequest {
    SystemOneRequest::new("I was charged twice.")
        .question("billing", noul("Is this about billing?"))
}

#[tokio::test]
async fn retries_5xx_then_parses_answer() {
    let (url, seen) = serve(vec![(500, "", r#"{"error":"boom"}"#), (200, "", OK_BODY)]).await;
    let result = client(&url).system_one(request()).await.unwrap();
    assert_eq!(result.answers["billing"].as_noul().unwrap().noul, 0.97);
    assert_eq!(result.usage.input_tokens, Some(12));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let lower = seen[1].to_ascii_lowercase();
    assert!(lower.contains("x-typesafe-retry-count: 1"));
    assert!(lower.contains("authorization: bearer sk-test-1234567890"));
    assert!(
        !seen[0]
            .to_ascii_lowercase()
            .contains("x-typesafe-retry-count")
    );
}

#[tokio::test]
async fn does_not_retry_401() {
    let (url, seen) = serve(vec![(
        401,
        "x-typesafe-request-id: req_1\r\n",
        r#"{"error":"bad key"}"#,
    )])
    .await;
    let Err(Error::Api(e)) = client(&url).system_one(request()).await else {
        panic!("expected API error")
    };
    assert_eq!((e.kind, e.status), (ApiErrorKind::Authentication, 401));
    assert_eq!(e.to_string(), "401 bad key");
    assert_eq!(e.request_id.as_deref(), Some("req_1"));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let (url, seen) = serve(vec![(503, "", ""), (503, "", ""), (503, "", "")]).await;
    let Err(Error::Api(e)) = client(&url).system_one(request()).await else {
        panic!("expected API error")
    };
    assert_eq!(e.kind, ApiErrorKind::InternalServer);
    assert_eq!(seen.lock().unwrap().len(), 3); // initial + 2 retries
}

#[tokio::test]
async fn lists_models() {
    let body =
        r#"{"models":[{"name":"jev-latest","description":"d","release_date":"2026-01-01"}]}"#;
    let (url, _) = serve(vec![(200, "", body)]).await;
    let models = client(&url).list_models().await.unwrap();
    assert_eq!(models[0].name, "jev-latest");
}

#[tokio::test]
async fn sends_image_as_multipart_and_compresses_large_json() {
    let png = Image::new([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1]).unwrap();
    let (url, seen) = serve(vec![(200, "", OK_BODY), (200, "", OK_BODY)]).await;
    let c = client(&url);
    let req = SystemOneRequest::new(format!("photo {png}"))
        .image(&png)
        .question("billing", noul("Is it damaged?"));
    c.system_one(req).await.unwrap();
    assert!(seen.lock().unwrap()[0].contains("multipart/form-data; boundary="));

    c.system_one(SystemOneRequest::new("x".repeat(5000)).question("billing", noul("q")))
        .await
        .unwrap();
    assert!(
        seen.lock().unwrap()[1]
            .to_ascii_lowercase()
            .contains("content-encoding: zstd")
    );
}

#[tokio::test]
async fn rejects_invalid_requests_before_sending() {
    let c = client("http://127.0.0.1:1");
    assert!(matches!(
        c.system_one(SystemOneRequest::new("s")).await,
        Err(Error::Sdk(_))
    ));
}

#[test]
fn missing_api_key_is_an_error() {
    // SAFETY: only this test touches the variable; the others pass an explicit key.
    unsafe { std::env::remove_var("TYPESAFE_API_KEY") };
    assert!(matches!(Client::new(), Err(Error::Sdk(_))));
}

#[test]
fn debug_output_hides_secrets() {
    let b = Client::builder()
        .api_key("sk-SECRET")
        .default_header("x-token", "hdr-SECRET");
    let o = typesafe_sdk::RequestOptions::default().header("x-token", "hdr-SECRET");
    for text in [
        format!("{b:?}"),
        format!("{o:?}"),
        format!("{:?}", b.build().unwrap()),
    ] {
        assert!(!text.contains("SECRET"), "{text}");
    }
}

#[tokio::test]
async fn null_state_is_rejected() {
    let c = client("http://127.0.0.1:1");
    let req = SystemOneRequest::new(serde_json::Value::Null).question("q", noul("x"));
    assert!(matches!(c.system_one(req).await, Err(Error::Sdk(_))));
}

#[tokio::test]
async fn absurd_retry_after_does_not_panic() {
    let (url, seen) = serve(vec![
        (429, "retry-after: 1e300\r\n", ""),
        (200, "", OK_BODY),
    ])
    .await;
    client(&url).system_one(request()).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
}

struct Capture(Mutex<Vec<String>>);

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        self.0.lock().unwrap().push(format!("{}", record.args()));
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture(Mutex::new(Vec::new()));

#[tokio::test]
async fn logs_attempts_and_redacts_the_key() {
    log::set_logger(&CAPTURE).ok();
    log::set_max_level(log::LevelFilter::Debug);
    let (url, _) = serve(vec![
        (503, "x-typesafe-request-id: req_9\r\n", ""),
        (200, "", OK_BODY),
    ])
    .await;
    client(&url).system_one(request()).await.unwrap();
    let lines = CAPTURE.0.lock().unwrap().join("\n");
    assert!(lines.contains("POST /v1/systemone <- 503"), "{lines}");
    assert!(lines.contains("(request req_9)"), "{lines}");
    assert!(
        lines.contains("retrying in") && lines.contains("(retry 1/2) after 503"),
        "{lines}"
    );
    assert!(lines.contains("authorization: Bearer ***7890"), "{lines}");
    assert!(!lines.contains("sk-test-1234567890"), "{lines}");
}

#[tokio::test]
async fn per_call_retry_overrides_merge_with_the_client_policy() {
    let (url, seen) = serve(vec![(503, "", ""), (503, "", "")]).await;
    let options = typesafe_sdk::RequestOptions::default().retry(typesafe_sdk::RetryOverrides {
        max_retries: Some(1),
        ..Default::default()
    });
    // The client's fast backoff still applies; only max_retries changed.
    assert!(
        client(&url)
            .system_one_with(request(), &options)
            .await
            .is_err()
    );
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn retry_after_http_date_in_the_past_retries_immediately() {
    let date = "retry-after: Thu, 01 Jan 2026 00:00:00 GMT\r\n";
    let (url, seen) = serve(vec![(429, date, ""), (200, "", OK_BODY)]).await;
    client(&url).system_one(request()).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
}

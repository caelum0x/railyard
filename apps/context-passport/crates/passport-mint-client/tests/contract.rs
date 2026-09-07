//! Contract/shape tests for the real `MintClient` against an in-process fake
//! HTTP transport (a one-shot `TcpListener`) — no live Mint required.
//!
//! Proves the wire contract the Mint reqwest client actually speaks: it POSTs to
//! `base + path`, carries the `X-API-Key` header, serializes the
//! `MintMeterRequest` body, parses a `MintMeterResult` on success, and maps a
//! non-2xx response to `PassportError::Mint` with the status in the message.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use passport_core::{MintMeter, MintMeterRequest, PassportError};
use passport_mint_client::MintClient;

/// Parse the `Content-Length` header value from a raw HTTP request head.
fn content_length(request: &str) -> Option<usize> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").or(line.strip_prefix("Content-Length:")))
        .and_then(|v| v.trim().parse().ok())
}

/// Bind an ephemeral port and serve exactly one request with `status_line` +
/// `response_body`. Returns the base URL and a channel delivering the full raw
/// request (headers + body) the client sent.
fn spawn_fake_mint(status_line: &'static str, response_body: &'static str) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let mut request = String::from_utf8_lossy(&buf[..n]).into_owned();
        // Read the remainder of the body if it did not arrive in the first read.
        if let Some(cl) = content_length(&request) {
            let header_end = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(request.len());
            let have = request.len().saturating_sub(header_end);
            if have < cl {
                let mut rest = vec![0u8; cl - have];
                if stream.read_exact(&mut rest).is_ok() {
                    request.push_str(&String::from_utf8_lossy(&rest));
                }
            }
        }
        let response = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        let _ = tx.send(request);
    });
    (format!("http://{addr}"), rx)
}

fn sample_request() -> MintMeterRequest {
    MintMeterRequest {
        reference: "gen_ext_1".into(),
        passport_id: "psp_abc".into(),
        version: 3,
        provider: "runway".into(),
        modality: "image".into(),
    }
}

#[tokio::test]
async fn meter_posts_expected_shape_and_parses_receipt() {
    let (base, rx) = spawn_fake_mint("HTTP/1.1 200 OK", "{\"receipt_ref\":\"rcpt_42\"}");
    let client = MintClient::new(base, "secret-key");

    let result = client.meter(&sample_request()).await.expect("meter succeeds");
    assert_eq!(result.receipt_ref.as_deref(), Some("rcpt_42"));

    let raw = rx.recv_timeout(Duration::from_secs(5)).expect("captured request");
    assert!(raw.starts_with("POST /v1/meter "), "posts to the meter path: {raw}");
    assert!(
        raw.to_lowercase().contains("x-api-key: secret-key"),
        "sends the api key header: {raw}"
    );
    assert!(raw.contains("\"reference\":\"gen_ext_1\""), "serializes reference: {raw}");
    assert!(raw.contains("\"passport_id\":\"psp_abc\""), "serializes passport id: {raw}");
    assert!(raw.contains("\"version\":3"), "serializes version: {raw}");
}

#[tokio::test]
async fn meter_honors_custom_path() {
    let (base, rx) = spawn_fake_mint("HTTP/1.1 200 OK", "{\"receipt_ref\":null}");
    let client = MintClient::new(base, "k").with_path("/internal/meter");

    let result = client.meter(&sample_request()).await.expect("meter succeeds");
    assert_eq!(result.receipt_ref, None);

    let raw = rx.recv_timeout(Duration::from_secs(5)).expect("captured request");
    assert!(raw.starts_with("POST /internal/meter "), "uses the overridden path: {raw}");
}

#[tokio::test]
async fn meter_maps_non_success_status_to_mint_error() {
    let (base, _rx) = spawn_fake_mint("HTTP/1.1 500 Internal Server Error", "boom");
    let client = MintClient::new(base, "k");

    let err = client.meter(&sample_request()).await.expect_err("non-2xx is an error");
    match err {
        PassportError::Mint(msg) => {
            assert!(msg.contains("500"), "error carries the status: {msg}");
            assert!(msg.contains("boom"), "error carries the body: {msg}");
        }
        other => panic!("expected PassportError::Mint, got {other:?}"),
    }
}

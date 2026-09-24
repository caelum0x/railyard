//! `HttpMintTransport` tests against a hand-rolled, in-process mock mint-server.
//!
//! The mock is a tiny tokio TCP stub (no `wiremock`/`httpmock` dependency, so the
//! suite runs fully offline and never touches a live Mint). Each test asserts the
//! client sends the right method / path / `x-api-key` / `idem_key` / body, parses
//! the `{success,data}` envelope into the crate's typed receipts, and folds each
//! error status onto the right [`GameError`] variant — with special attention to
//! the terminal-hold -> `HoldNotFound`, `402` -> `InsufficientFunds`, and `409` ->
//! `IdempotencyConflict` mappings the ledger relies on.

use std::sync::{Arc, Mutex};

use game_economy_core::error::GameError;
use game_economy_core::transport::{Capture, Hold, MintTransport, Release};
use game_economy_core::{Catalog, GameStore};
use game_economy_mint_client::HttpMintTransport;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const API_KEY: &str = "test-secret-key";

// ---- the mock mint-server --------------------------------------------------

/// One captured HTTP request.
#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// Start a mock mint-server on an ephemeral port. `handler` maps each request to a
/// `(status, json_body)` response; every request is recorded into the returned log.
/// The server loops in a background task for the lifetime of the test runtime.
async fn start_mock<F>(handler: F) -> (String, Arc<Mutex<Vec<Captured>>>)
where
    F: Fn(&Captured) -> (u16, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let log_bg = log.clone();
    let handler = Arc::new(handler);

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let log_conn = log_bg.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Some(req) = read_request(&mut sock).await {
                    let (status, body) = handler(&req);
                    log_conn.lock().unwrap().push(req);
                    let _ = write_response(&mut sock, status, &body).await;
                }
            });
        }
    });

    (format!("http://{addr}"), log)
}

/// Read a single HTTP/1.1 request (headers + Content-Length body) off the socket.
async fn read_request(sock: &mut TcpStream) -> Option<Captured> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];

    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut rl = request_line.split_whitespace();
    let method = rl.next()?.to_string();
    let path = rl.next()?.to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    Some(Captured {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

async fn write_response(sock: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Status",
    };
    // Successful terminal hold responses carry the same flat signed evidence
    // header as mint-server. The client must use this as its canonical anchor,
    // never derive a receipt locally from response fields.
    const CAPTURE_RECEIPT: &str = "eyJzZXEiOjQyLCJvcCI6ImhvbGQuY2FwdHVyZSIsIm9wX2hhc2giOiJhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhIiwiYW1vdW50X21pY3JvcyI6OTkwMDAwLCJiYWxhbmNlX2FmdGVyX21pY3JvcyI6NzAxMDAwMCwicHJldl9oYXNoIjoiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYiIsImhhc2giOiJjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjIiwic2lnIjoiZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWUifQ";
    const RELEASE_RECEIPT: &str = "eyJzZXEiOjksIm9wIjoiaG9sZC5yZWxlYXNlIiwib3BfaGFzaCI6ImFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWEiLCJhbW91bnRfbWljcm9zIjo5OTAwMDAsImJhbGFuY2VfYWZ0ZXJfbWljcm9zIjo3MDEwMDAwLCJwcmV2X2hhc2giOiJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiIiwiaGFzaCI6ImRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGRkZGQiLCJzaWciOiJlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZSJ9";
    let receipt_header = if (200..300).contains(&status) && body.contains("\"status\":\"released\"")
    {
        format!("AITP-Receipt: {RELEASE_RECEIPT}\r\nAITP-Version: 1\r\n")
    } else if (200..300).contains(&status) && body.contains("\"captured_micros\"") {
        format!("AITP-Receipt: {CAPTURE_RECEIPT}\r\nAITP-Version: 1\r\n")
    } else {
        String::new()
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{receipt_header}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.flush().await
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A `{success:true,data:<data>}` envelope.
fn ok_envelope(data: serde_json::Value) -> String {
    serde_json::json!({ "success": true, "data": data }).to_string()
}

/// A `{success:false,error:{code,message}}` envelope.
fn err_envelope(code: &str, message: &str) -> String {
    serde_json::json!({
        "success": false,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

fn client(base: &str) -> HttpMintTransport {
    HttpMintTransport::new(base, API_KEY).expect("build client")
}

// ---- request-shape + happy-path parsing ------------------------------------

#[tokio::test]
async fn create_hold_posts_correct_request_and_parses_receipt() {
    let (base, log) = start_mock(|_req| {
        (
            201,
            ok_envelope(serde_json::json!({
                "hold_id": "hold_00000000000000000000000001",
                "first_seq": 5,
                "status": "open",
                "captured_micros": 0,
                "idempotent_replay": false
            })),
        )
    })
    .await;

    let receipt = client(&base)
        .create_hold(Hold {
            account_id: "acct_player".to_string(),
            amount_micros: 990_000,
            idem_key: "buy-1:hold".to_string(),
        })
        .await
        .expect("create_hold");

    assert_eq!(receipt.hold_id, "hold_00000000000000000000000001");
    // The reserved amount is echoed from the request (view omits it).
    assert_eq!(receipt.amount_micros, 990_000);
    assert!(!receipt.idempotent_replay);

    let reqs = log.lock().unwrap();
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/holds");
    assert_eq!(r.header("x-api-key"), Some(API_KEY));
    let body = r.json();
    assert_eq!(body["from"], "acct_player");
    assert_eq!(body["amount_micros"], 990_000);
    assert_eq!(body["idem_key"], "buy-1:hold");
}

#[tokio::test]
async fn capture_hold_uses_signed_receipt_as_entitlement_anchor() {
    let hold_id = "hold_00000000000000000000000007";
    let (base, log) = start_mock(|_req| {
        (
            200,
            ok_envelope(serde_json::json!({
                "hold_id": "hold_00000000000000000000000007",
                "first_seq": 42,
                "holder_balance": { "available_micros": 7_010_000 },
                "payee_balance": { "available_micros": 990_000 },
                "status": "captured",
                "captured_micros": 990_000,
                "idempotent_replay": false
            })),
        )
    })
    .await;

    let receipt = client(&base)
        .capture_hold(
            hold_id,
            Capture {
                amount_micros: 990_000,
                to: "acct_revenue".to_string(),
                idem_key: "buy-1:capture".to_string(),
            },
        )
        .await
        .expect("capture_hold");

    assert_eq!(receipt.captured_micros, 990_000);
    assert_eq!(receipt.balance_after_micros, 7_010_000);
    assert_eq!(receipt.settlement_seq, 42);
    assert!(!receipt.idempotent_replay);
    assert!(!receipt.receipt_id.is_empty());
    assert_eq!(receipt.proof.operation, "hold.capture");
    assert_eq!(receipt.proof.sequence, 42);
    assert_eq!(receipt.proof.receipt_hash, receipt.receipt_id);
    assert!(!receipt.proof.signature.is_empty());

    let reqs = log.lock().unwrap();
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/holds/hold_00000000000000000000000007/capture");
    assert_eq!(r.header("x-api-key"), Some(API_KEY));
    let body = r.json();
    assert_eq!(body["amount_micros"], 990_000);
    assert_eq!(body["to"], "acct_revenue");
    assert_eq!(body["idem_key"], "buy-1:capture");
}

#[tokio::test]
async fn release_hold_posts_idem_key_and_parses_replay_flag() {
    let (base, log) = start_mock(|_req| {
        (
            200,
            ok_envelope(serde_json::json!({
                "hold_id": "hold_00000000000000000000000007",
                "first_seq": 9,
                "status": "released",
                "captured_micros": 0,
                "idempotent_replay": true
            })),
        )
    })
    .await;

    let receipt = client(&base)
        .release_hold(
            "hold_00000000000000000000000007",
            Release {
                idem_key: "buy-1:release".to_string(),
            },
        )
        .await
        .expect("release_hold");

    assert!(receipt.idempotent_replay);
    assert_eq!(receipt.proof.operation, "hold.release");

    let reqs = log.lock().unwrap();
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/holds/hold_00000000000000000000000007/release");
    assert_eq!(r.header("x-api-key"), Some(API_KEY));
    assert_eq!(r.json()["idem_key"], "buy-1:release");
}

#[tokio::test]
async fn idempotent_replay_flag_is_parsed_on_capture() {
    let (base, _log) = start_mock(|_req| {
        (
            200,
            ok_envelope(serde_json::json!({
                "hold_id": "hold_x",
                "first_seq": 1,
                "holder_balance": { "available_micros": 0 },
                "captured_micros": 990_000,
                "idempotent_replay": true
            })),
        )
    })
    .await;

    let receipt = client(&base)
        .capture_hold(
            "hold_x",
            Capture {
                amount_micros: 990_000,
                to: "acct_revenue".to_string(),
                idem_key: "buy-1:capture".to_string(),
            },
        )
        .await
        .expect("capture_hold replay");
    assert!(receipt.idempotent_replay);
}

// ---- error-status -> typed-error mapping -----------------------------------

#[tokio::test]
async fn insufficient_funds_402_maps_to_insufficient_funds() {
    let (base, _log) = start_mock(|_req| {
        (
            402,
            err_envelope("insufficient_funds", "available balance too low"),
        )
    })
    .await;

    let err = client(&base)
        .create_hold(Hold {
            account_id: "acct_player".to_string(),
            amount_micros: 999_999_999,
            idem_key: "buy-1:hold".to_string(),
        })
        .await
        .expect_err("should be insufficient funds");
    assert_eq!(err, GameError::InsufficientFunds);
}

#[tokio::test]
async fn terminal_hold_404_maps_to_hold_not_found() {
    // Mint collapses a distinct close of an already-terminal hold to hold_not_found.
    let (base, _log) =
        start_mock(|_req| (404, err_envelope("hold_not_found", "no such hold"))).await;

    let err = client(&base)
        .capture_hold(
            "hold_terminal",
            Capture {
                amount_micros: 990_000,
                to: "acct_revenue".to_string(),
                idem_key: "buy-1:capture".to_string(),
            },
        )
        .await
        .expect_err("should be hold not found");
    assert_eq!(err, GameError::HoldNotFound);
}

#[tokio::test]
async fn idempotency_conflict_409_maps_to_idempotency_conflict() {
    let (base, _log) = start_mock(|_req| {
        (
            409,
            err_envelope("idempotency_conflict", "same key, different body"),
        )
    })
    .await;

    let err = client(&base)
        .create_hold(Hold {
            account_id: "acct_player".to_string(),
            amount_micros: 990_000,
            idem_key: "buy-1:hold".to_string(),
        })
        .await
        .expect_err("should be idempotency conflict");
    assert_eq!(err, GameError::IdempotencyConflict);
}

#[tokio::test]
async fn capture_exceeds_hold_422_maps_to_capture_exceeds_hold() {
    let (base, _log) =
        start_mock(|_req| (422, err_envelope("capture_exceeds_hold", "amount > hold"))).await;

    let err = client(&base)
        .capture_hold(
            "hold_x",
            Capture {
                amount_micros: 2_000_000,
                to: "acct_revenue".to_string(),
                idem_key: "buy-1:capture".to_string(),
            },
        )
        .await
        .expect_err("should be capture exceeds hold");
    assert_eq!(err, GameError::CaptureExceedsHold);
}

#[tokio::test]
async fn frozen_403_maps_to_typed_mint_error() {
    let (base, _log) =
        start_mock(|_req| (403, err_envelope("account_frozen", "account is frozen"))).await;

    let err = client(&base)
        .create_hold(Hold {
            account_id: "acct_frozen".to_string(),
            amount_micros: 990_000,
            idem_key: "buy-1:hold".to_string(),
        })
        .await
        .expect_err("should be a typed mint error, never a raw throw");
    match err {
        GameError::Mint(m) => assert!(m.contains("frozen")),
        other => panic!("expected Mint(..), got {other:?}"),
    }
}

#[tokio::test]
async fn status_only_402_without_code_still_maps() {
    // Body carries no recognizable code — the status fallback must still classify.
    let (base, _log) = start_mock(|_req| (402, "{\"success\":false}".to_string())).await;

    let err = client(&base)
        .create_hold(Hold {
            account_id: "acct_player".to_string(),
            amount_micros: 990_000,
            idem_key: "buy-1:hold".to_string(),
        })
        .await
        .expect_err("status fallback");
    assert_eq!(err, GameError::InsufficientFunds);
}

// ---- end-to-end: a real GameStore settling over HTTP -----------------------

/// The strongest proof: drive a full `GameStore::purchase` over the HTTP transport
/// against a routed mock mint-server. This is the exact flow prod runs, only the
/// server is in-process — a real hold -> capture -> grant, receipted over HTTP.
#[tokio::test]
async fn game_store_settles_a_purchase_over_http_end_to_end() {
    let (base, log) = start_mock(|req| {
        if req.path == "/v1/holds" {
            (
                201,
                ok_envelope(serde_json::json!({
                    "hold_id": "hold_00000000000000000000000042",
                    "first_seq": 100,
                    "status": "open",
                    "captured_micros": 0,
                    "idempotent_replay": false
                })),
            )
        } else if req.path.ends_with("/capture") {
            (
                200,
                ok_envelope(serde_json::json!({
                    "hold_id": "hold_00000000000000000000000042",
                    "first_seq": 101,
                    "holder_balance": { "available_micros": 9_010_000 },
                    "payee_balance": { "available_micros": 990_000 },
                    "status": "captured",
                    "captured_micros": 990_000,
                    "idempotent_replay": false
                })),
            )
        } else {
            (500, err_envelope("internal", "unexpected route"))
        }
    })
    .await;

    let store = GameStore::new(client(&base), Catalog::demo(), "acct_revenue");
    let out = store
        .purchase("acct_player", "pack.standard", "buy-42")
        .await
        .expect("purchase settles over http");

    // Charged the exact catalog price and granted the pack.
    assert_eq!(out.charged_micros, 990_000);
    assert_eq!(out.settlement_seq, 42);
    assert!(!out.idempotent_replay);
    assert_eq!(out.receipt_id, out.settlement_proof.receipt_hash);

    let inv = store.inventory("acct_player");
    assert_eq!(inv.packs.len(), 1);
    let pack = inv.packs.get(&out.entitlement_id).expect("pack granted");
    assert_eq!(pack.pack_sku, "pack.standard");
    assert!(!pack.is_opened());

    // The store spread ONE idem_key across the triple with :hold / :capture
    // suffixes, sent verbatim over the wire (proving no double-suffixing).
    let reqs = log.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].path, "/v1/holds");
    assert_eq!(reqs[0].json()["idem_key"], "buy-42:hold");
    assert!(reqs[1].path.ends_with("/capture"));
    assert_eq!(reqs[1].json()["idem_key"], "buy-42:capture");
    assert_eq!(reqs[1].json()["to"], "acct_revenue");
}

// ---- read_balance ------------------------------------------------------------

#[tokio::test]
async fn read_balance_gets_account_balance_with_api_key() {
    let (base, log) = start_mock(|_req| {
        (
            200,
            ok_envelope(serde_json::json!({
                "account_id": "acct_player",
                "available_micros": 4_990_000,
                "held_micros": 990_000,
                "updated_seq": 7
            })),
        )
    })
    .await;

    let bal = client(&base).read_balance("acct_player").await.expect("balance");
    assert_eq!(bal.account_id, "acct_player");
    assert_eq!(bal.available_micros, 4_990_000);
    assert_eq!(bal.held_micros, 990_000);

    let reqs = log.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/v1/accounts/acct_player/balance");
    assert_eq!(reqs[0].header("x-api-key"), Some(API_KEY));
}

#[tokio::test]
async fn read_balance_unknown_account_is_zero() {
    let (base, _log) =
        start_mock(|_req| (404, err_envelope("account_not_found", "no such account"))).await;
    let bal = client(&base).read_balance("acct_new").await.expect("balance");
    assert_eq!(bal.available_micros, 0);
    assert_eq!(bal.held_micros, 0);
}

#[tokio::test]
async fn read_balance_rejects_path_unsafe_ids_without_a_request() {
    let (base, log) = start_mock(|_req| (200, ok_envelope(serde_json::json!({})))).await;
    let err = client(&base)
        .read_balance("acct/../admin")
        .await
        .expect_err("must reject");
    assert!(matches!(err, GameError::Mint(_)));
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_player_account_posts_user_kind() {
    let (base, log) = start_mock(|_req| {
        (
            201,
            ok_envelope(serde_json::json!({ "account_id": "acct_001ARZ3NDEKTSV4RRFFQ69G5FAV" })),
        )
    })
    .await;
    let id = client(&base)
        .create_player_account("flowblast:p1")
        .await
        .expect("create");
    assert_eq!(id, "acct_001ARZ3NDEKTSV4RRFFQ69G5FAV");
    let reqs = log.lock().unwrap().clone();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/accounts");
    assert_eq!(reqs[0].json()["kind"], serde_json::json!("User"));
    assert_eq!(reqs[0].json()["owner_ref"], serde_json::json!("flowblast:p1"));
}

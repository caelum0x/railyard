//! A record-and-replay fake [`HttpTransport`] for exercising [`MintLedger`]
//! without a live mint-server.
//!
//! It records every [`HttpRequest`] it receives (so tests can assert the EXACT
//! holds/capture/release calls) and answers with canned, contract-shaped
//! mint-server envelopes. It is intentionally `pub` (not `#[cfg(test)]`) so the
//! gateway crate can drive the billing middleware end-to-end over a fake mint in
//! its own tests.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

/// A canned mint-server that records requests and replays contract responses.
pub struct FakeMint {
    seen: Mutex<Vec<HttpRequest>>,
    hold_id: String,
    /// When `true`, `POST /v1/holds` answers `402 insufficient_funds`.
    insufficient: bool,
    /// The `available_micros` reported by `GET /v1/accounts/:id/balance`.
    balance_micros: i64,
}

impl FakeMint {
    /// A funded fake that grants holds and reports `balance_micros` available.
    pub fn funded(balance_micros: i64) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            hold_id: "hold_01FAKE".to_string(),
            insufficient: false,
            balance_micros,
        }
    }

    /// A fake whose `POST /v1/holds` always returns `402 insufficient_funds`.
    pub fn insufficient() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            hold_id: "hold_01FAKE".to_string(),
            insufficient: true,
            balance_micros: 0,
        }
    }

    /// Override the `hold_id` handed back on `POST /v1/holds`.
    #[must_use]
    pub fn with_hold_id(mut self, hold_id: impl Into<String>) -> Self {
        self.hold_id = hold_id.into();
        self
    }

    /// A snapshot of every request received, in order.
    pub fn seen(&self) -> Vec<HttpRequest> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// All requests matching `method` whose path ends with `path_suffix`.
    pub fn matching(&self, method: &str, path_suffix: &str) -> Vec<HttpRequest> {
        self.seen()
            .into_iter()
            .filter(|r| r.method == method && r.path.ends_with(path_suffix))
            .collect()
    }

    /// Wait (up to `timeout`) until at least one request matches `method` +
    /// `path_suffix`, returning that request. Drives the runtime so fire-and-forget
    /// spawned terminal ops (the middleware release path) become observable.
    pub async fn wait_for(
        &self,
        method: &str,
        path_suffix: &str,
        timeout: Duration,
    ) -> Option<HttpRequest> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(found) = self.matching(method, path_suffix).into_iter().next() {
                return Some(found);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn record(&self, req: &HttpRequest) {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.clone());
    }

    fn respond(&self, req: &HttpRequest) -> HttpResponse {
        let path = req.path.as_str();
        match (req.method, path) {
            ("POST", "/v1/holds") if self.insufficient => error_response(
                402,
                "insufficient_funds",
                "account has insufficient available balance",
            ),
            ("POST", "/v1/holds") => ok_response(
                201,
                json!({
                    "hold_id": self.hold_id,
                    "first_seq": 1,
                    "status": "open",
                    "captured_micros": 0,
                    "idempotent_replay": false,
                }),
            ),
            ("POST", p) if p.starts_with("/v1/holds/") && p.ends_with("/capture") => ok_response(
                200,
                json!({
                    "id": self.hold_id,
                    "first_seq": 2,
                    "status": "captured",
                    "captured_micros": captured_from_body(&req.body),
                    "holder_balance": { "available_micros": self.balance_micros, "held_micros": 0 },
                }),
            ),
            ("POST", p) if p.starts_with("/v1/holds/") && p.ends_with("/release") => ok_response(
                200,
                json!({
                    "id": self.hold_id,
                    "first_seq": 2,
                    "status": "released",
                    "captured_micros": 0,
                }),
            ),
            ("GET", p) if p.starts_with("/v1/accounts/") && p.ends_with("/balance") => ok_response(
                200,
                json!({
                    "account_id": "acct_fake",
                    "available_micros": self.balance_micros,
                    "held_micros": 0,
                    "updated_seq": 0,
                }),
            ),
            _ => error_response(404, "not_found", "no fake route"),
        }
    }
}

#[async_trait]
impl HttpTransport for FakeMint {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.record(&req);
        Ok(self.respond(&req))
    }
}

fn captured_from_body(body: &[u8]) -> i64 {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("amount_micros").and_then(Value::as_i64))
        .unwrap_or(0)
}

fn ok_response(status: u16, data: Value) -> HttpResponse {
    let body = json!({ "success": true, "data": data });
    HttpResponse::new(status, serde_json::to_vec(&body).unwrap_or_default())
}

fn error_response(status: u16, code: &str, message: &str) -> HttpResponse {
    let body = json!({
        "success": false,
        "error": { "code": code, "message": message },
    });
    HttpResponse::new(status, serde_json::to_vec(&body).unwrap_or_default())
}

//! The one error envelope: `{ "ok": false, "error": { "code", "message", "details" } }`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// An API-visible error: HTTP status, stable machine code, human message and
/// structured details (JSON Pointer errors, cap names, balances, fallback chain).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{status} {code}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        ApiError { status, code: code.to_string(), message: message.into(), details: Value::Null }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, "unauthorized", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "forbidden", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "not_found", message)
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "bad_request", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal", message)
    }
    /// 402: the prepaid balance cannot cover the quote.
    pub fn payment_required(balance_micros: i64, estimated_cost_micros: i64, topup_url: &str) -> Self {
        Self::new(402, "insufficient_balance", "Balance is too low for this call. Top up and retry.").with_details(json!({
            "balance_micros": balance_micros,
            "estimated_cost_micros": estimated_cost_micros,
            "topup_url": topup_url,
        }))
    }
    /// 429 `cap_exceeded`, naming the cap that stopped the call.
    pub fn cap_exceeded(cap: &str, limit_micros: i64, spent_micros: i64, estimated_cost_micros: i64) -> Self {
        Self::new(429, "cap_exceeded", format!("The {cap} spend cap would be exceeded by this call.")).with_details(json!({
            "cap": cap,
            "limit_micros": limit_micros,
            "spent_micros": spent_micros,
            "estimated_cost_micros": estimated_cost_micros,
        }))
    }
    /// 422: schema validation failed; `errors` are `{pointer, message}` pairs.
    pub fn validation(errors: Vec<(String, String)>) -> Self {
        let list: Vec<Value> = errors.into_iter().map(|(p, m)| json!({"pointer": p, "message": m})).collect();
        Self::new(422, "invalid_input", "Input does not match the tool's JSON Schema. Nothing was spent.")
            .with_details(json!({ "errors": list }))
    }
    /// 409: idempotency key reused with a different body.
    pub fn idempotency_conflict() -> Self {
        Self::new(409, "idempotency_conflict", "This Idempotency-Key was already used with a different request body.")
    }
    /// 424: every candidate in the chain failed; nothing billed.
    pub fn chain_failed(chain: Value) -> Self {
        Self::new(424, "all_providers_failed", "Every provider in the fallback chain failed. Nothing was billed.")
            .with_details(json!({ "fallback_chain": chain }))
    }

    /// The JSON envelope body.
    pub fn body(&self) -> Value {
        json!({ "ok": false, "error": { "code": self.code, "message": self.message, "details": self.details } })
    }
}

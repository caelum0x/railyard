//! Mint: the balance of record for prepaid credit.

use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MintError {
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("account frozen")]
    AccountFrozen,
    #[error("account not found")]
    AccountNotFound,
    #[error("hold not found")]
    HoldNotFound,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("mint unavailable: {0}")]
    Unavailable(String),
    #[error("mint error {status} {code}: {message}")]
    Other { status: u16, code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintHold {
    pub hold_id: String,
    pub amount_micros: i64,
}

/// A terminal settlement with Mint's signed, hash-linked receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintReceipt {
    pub amount_micros: i64,
    /// `<seq>:<receipt_hash>` of the AITP receipt (empty when Mint sent none).
    pub receipt_id: String,
    pub receipt: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintBalance {
    pub available_micros: i64,
    pub held_micros: i64,
}

#[async_trait]
pub trait MintTransport: Send + Sync {
    async fn create_hold(&self, account_id: &str, amount_micros: i64, idem_key: &str) -> Result<MintHold, MintError>;
    async fn capture(&self, hold_id: &str, amount_micros: i64, to_account: &str, idem_key: &str) -> Result<MintReceipt, MintError>;
    async fn release(&self, hold_id: &str, idem_key: &str) -> Result<MintReceipt, MintError>;
    async fn balance(&self, account_id: &str) -> Result<MintBalance, MintError>;
    /// Start a credit top-up checkout (Dodo by default) and return its URL.
    async fn checkout(&self, account_id: &str, amount_micros: i64, provider: &str, success_url: Option<&str>) -> Result<String, MintError>;
}

/// Decode Mint's `AITP-Receipt` header (base64url JSON) into `(receipt_id, json)`.
pub fn decode_receipt_header(header: Option<&str>) -> (String, Value) {
    let Some(h) = header else { return (String::new(), Value::Null) };
    let decoded = URL_SAFE_NO_PAD.decode(h.trim()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    match decoded {
        Some(v) => {
            let seq = v.get("seq").or_else(|| v.get("sequence")).cloned().unwrap_or(Value::Null);
            let hash = v.get("hash").or_else(|| v.get("receipt_hash")).and_then(Value::as_str).unwrap_or("");
            (format!("{seq}:{hash}"), v)
        }
        None => (String::new(), json!({ "raw": h })),
    }
}

/// HTTP client for the live mint-server (`X-API-Key` auth).
#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

impl MintClient {
    pub fn new(base: &str, api_key: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("reqwest client");
        MintClient { http, base: base.trim_end_matches('/').to_string(), api_key: api_key.to_string() }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<(Value, Option<String>), MintError> {
        let resp = req.header("X-API-Key", &self.api_key).send().await.map_err(|e| MintError::Unavailable(e.to_string()))?;
        let status = resp.status().as_u16();
        let receipt = resp.headers().get("AITP-Receipt").and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = resp.text().await.map_err(|e| MintError::Unavailable(e.to_string()))?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok((body.get("data").cloned().unwrap_or(body), receipt));
        }
        let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("").to_string();
        let message = body.pointer("/error/message").and_then(Value::as_str).unwrap_or(&text).to_string();
        Err(match (status, code.as_str()) {
            (_, "insufficient_funds") | (402, _) => MintError::InsufficientFunds,
            (_, "account_frozen") | (403, _) => MintError::AccountFrozen,
            (_, "account_not_found") => MintError::AccountNotFound,
            (_, "hold_not_found") => MintError::HoldNotFound,
            (_, "idempotency_conflict") | (409, _) => MintError::IdempotencyConflict,
            (500..=599, _) => MintError::Unavailable(format!("{status} {message}")),
            _ => MintError::Other { status, code, message },
        })
    }
}

#[async_trait]
impl MintTransport for MintClient {
    async fn create_hold(&self, account_id: &str, amount_micros: i64, idem_key: &str) -> Result<MintHold, MintError> {
        let body = json!({ "from": account_id, "amount_micros": amount_micros, "idem_key": format!("{idem_key}:hold") });
        let (data, _) = self.send(self.http.post(format!("{}/v1/holds", self.base)).json(&body)).await?;
        let hold_id = data.get("hold_id").or_else(|| data.get("id")).and_then(Value::as_str).unwrap_or_default().to_string();
        if hold_id.is_empty() {
            return Err(MintError::Unavailable("mint hold response missing hold_id".into()));
        }
        Ok(MintHold { hold_id, amount_micros })
    }

    async fn capture(&self, hold_id: &str, amount_micros: i64, to_account: &str, idem_key: &str) -> Result<MintReceipt, MintError> {
        let body = json!({ "amount_micros": amount_micros, "to": to_account, "idem_key": format!("{idem_key}:capture") });
        let (data, header) = self.send(self.http.post(format!("{}/v1/holds/{hold_id}/capture", self.base)).json(&body)).await?;
        let (receipt_id, receipt) = decode_receipt_header(header.as_deref());
        Ok(MintReceipt { amount_micros: data.get("captured_micros").and_then(Value::as_i64).unwrap_or(amount_micros), receipt_id, receipt })
    }

    async fn release(&self, hold_id: &str, idem_key: &str) -> Result<MintReceipt, MintError> {
        let body = json!({ "idem_key": format!("{idem_key}:release") });
        let (data, header) = self.send(self.http.post(format!("{}/v1/holds/{hold_id}/release", self.base)).json(&body)).await?;
        let (receipt_id, receipt) = decode_receipt_header(header.as_deref());
        Ok(MintReceipt { amount_micros: data.get("released_micros").and_then(Value::as_i64).unwrap_or(0), receipt_id, receipt })
    }

    async fn balance(&self, account_id: &str) -> Result<MintBalance, MintError> {
        let (data, _) = self.send(self.http.get(format!("{}/v1/accounts/{account_id}/balance", self.base))).await?;
        Ok(MintBalance {
            available_micros: data.get("available_micros").and_then(Value::as_i64).unwrap_or(0),
            held_micros: data.get("held_micros").and_then(Value::as_i64).unwrap_or(0),
        })
    }

    async fn checkout(&self, account_id: &str, amount_micros: i64, provider: &str, success_url: Option<&str>) -> Result<String, MintError> {
        let body = json!({ "provider": provider, "account_id": account_id, "amount_micros": amount_micros, "success_url": success_url });
        let (data, _) = self.send(self.http.post(format!("{}/v1/checkouts", self.base)).json(&body)).await?;
        data.get("url")
            .or_else(|| data.get("checkout_url"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| MintError::Unavailable("checkout response missing url".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_aitp_receipt_header() {
        let r = json!({"seq": 42, "op": "capture", "op_hash": "oh", "hash": "abc", "sig": "s"});
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&r).unwrap());
        let (id, v) = decode_receipt_header(Some(&header));
        assert_eq!(id, "42:abc");
        assert_eq!(v["op"], "capture");
        assert_eq!(decode_receipt_header(None).0, "");
        assert_eq!(decode_receipt_header(Some("%%%")).1["raw"], "%%%");
    }
}

//! Mint hold client: atomic reserve / capture / release against a
//! `mint-server` (`apps/mint`), used by real-time prepaid wallets.
//!
//! Every call carries an idempotency key, so a retried request (after a
//! timeout, a crash or a duplicate event) settles exactly once at Mint. Mint
//! answers `{ success, data }` / `{ success: false, error: { code, message } }`
//! and signs every hold operation with a receipt (`aitp-receipt` header), which
//! Rago does not need to re-verify because it talks to its own Mint over an
//! authenticated channel (`x-api-key`).

use serde_json::{json, Value};

use crate::transport::{HttpTransport, TransportError};

/// A Mint hold as reported by create / capture / release / get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintHold {
    pub hold_id: String,
    /// `Open`, `Captured` or `Released`.
    pub status: String,
    pub captured_micros: i64,
    /// `true` when Mint recognized the idempotency key (a replay).
    pub idempotent_replay: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// 402 `insufficient_funds`: the payer cannot cover the hold.
    #[error("insufficient funds at mint")]
    InsufficientFunds,
    /// 404: unknown account or hold (or a terminal hold closed again).
    #[error("mint {0} not found")]
    NotFound(String),
    /// 409: the idempotency key was reused for a different request.
    #[error("mint idempotency conflict")]
    Conflict,
    /// Any other refusal.
    #[error("mint error {status} {code}: {message}")]
    Api { status: u16, code: String, message: String },
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("invalid mint response: {0}")]
    InvalidResponse(String),
}

/// Client for one Mint server.
pub struct MintHoldsClient<T: HttpTransport> {
    base_url: String,
    api_key: String,
    transport: T,
}

impl<T: HttpTransport> MintHoldsClient<T> {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            transport,
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    fn parse(status: u16, body: &str) -> Result<Value, MintError> {
        let v: Value = serde_json::from_str(body).map_err(|e| MintError::InvalidResponse(e.to_string()))?;
        if (200..300).contains(&status) && v["success"].as_bool() != Some(false) {
            return Ok(v["data"].clone());
        }
        let code = v["error"]["code"].as_str().unwrap_or("error").to_string();
        let message = v["error"]["message"].as_str().unwrap_or("").to_string();
        Err(match (status, code.as_str()) {
            (402, _) | (_, "insufficient_funds") => MintError::InsufficientFunds,
            (404, _) => MintError::NotFound(code),
            (409, "idempotency_conflict") => MintError::Conflict,
            _ => MintError::Api { status, code, message },
        })
    }

    fn hold_from(data: &Value, fallback_id: Option<&str>) -> Result<MintHold, MintError> {
        let hold_id = data["hold_id"]
            .as_str()
            .or_else(|| data["id"].as_str())
            .or(fallback_id)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MintError::InvalidResponse("missing hold_id".into()))?
            .to_string();
        Ok(MintHold {
            hold_id,
            status: data["status"].as_str().unwrap_or("Open").to_string(),
            captured_micros: data["captured_micros"].as_i64().unwrap_or(0),
            idempotent_replay: data["idempotent_replay"].as_bool().unwrap_or(false),
        })
    }

    /// `POST /v1/holds`: reserve `amount_micros` of `from`.
    pub async fn create_hold(
        &self,
        from: &str,
        amount_micros: i64,
        idem_key: &str,
        reference: &str,
    ) -> Result<MintHold, MintError> {
        let body = json!({
            "from": from,
            "amount_micros": amount_micros,
            "idem_key": idem_key,
            "ref": reference,
        });
        let resp = self
            .transport
            .post(&format!("{}/v1/holds", self.base_url), &self.headers(), body.to_string())
            .await?;
        Self::hold_from(&Self::parse(resp.status, &resp.body)?, None)
    }

    /// `POST /v1/holds/:id/capture`: settle `amount_micros` to `to`; Mint
    /// releases the uncaptured remainder.
    pub async fn capture(
        &self,
        hold_id: &str,
        amount_micros: i64,
        to: &str,
        idem_key: &str,
        reference: &str,
    ) -> Result<MintHold, MintError> {
        let body = json!({ "amount_micros": amount_micros, "to": to, "idem_key": idem_key, "ref": reference });
        let resp = self
            .transport
            .post(&format!("{}/v1/holds/{}/capture", self.base_url, path_segment(hold_id)), &self.headers(), body.to_string())
            .await?;
        Self::hold_from(&Self::parse(resp.status, &resp.body)?, Some(hold_id))
    }

    /// `POST /v1/holds/:id/release`: free the whole hold.
    pub async fn release(&self, hold_id: &str, idem_key: &str, reference: &str) -> Result<MintHold, MintError> {
        let body = json!({ "idem_key": idem_key, "ref": reference });
        let resp = self
            .transport
            .post(&format!("{}/v1/holds/{}/release", self.base_url, path_segment(hold_id)), &self.headers(), body.to_string())
            .await?;
        Self::hold_from(&Self::parse(resp.status, &resp.body)?, Some(hold_id))
    }

    /// `GET /v1/holds/:id`.
    pub async fn get_hold(&self, hold_id: &str) -> Result<MintHold, MintError> {
        let resp = self
            .transport
            .get(&format!("{}/v1/holds/{}", self.base_url, path_segment(hold_id)), &self.headers())
            .await?;
        Self::hold_from(&Self::parse(resp.status, &resp.body)?, Some(hold_id))
    }
}

fn path_segment(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, MockTransport};

    #[tokio::test]
    async fn create_hold_sends_the_idempotent_request() {
        let t = MockTransport::always(
            201,
            r#"{"success":true,"data":{"hold_id":"hold_1","first_seq":4,"status":"Open","captured_micros":0,"idempotent_replay":false}}"#,
        );
        let c = MintHoldsClient::new("https://mint.test/", "mk", &t);
        let hold = c.create_hold("acct_cust", 1_250_000, "rago:k", "INV").await.unwrap();
        assert_eq!(hold.hold_id, "hold_1");
        assert_eq!(hold.status, "Open");
        let sent = t.last_request().unwrap();
        assert_eq!(sent.url, "https://mint.test/v1/holds");
        assert!(sent.headers.iter().any(|(k, v)| k == "x-api-key" && v == "mk"));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["amount_micros"], 1_250_000);
        assert_eq!(body["idem_key"], "rago:k");
    }

    #[tokio::test]
    async fn insufficient_funds_and_not_found_are_typed() {
        let t = MockTransport::always(
            402,
            r#"{"success":false,"error":{"code":"insufficient_funds","message":"insufficient funds"}}"#,
        );
        let c = MintHoldsClient::new("https://mint.test", "mk", &t);
        assert!(matches!(c.create_hold("a", 1, "k", "r").await, Err(MintError::InsufficientFunds)));

        let t = MockTransport::always(404, r#"{"success":false,"error":{"code":"hold_not_found","message":""}}"#);
        let c = MintHoldsClient::new("https://mint.test", "mk", &t);
        assert!(matches!(c.release("h", "k", "r").await, Err(MintError::NotFound(_))));
    }

    #[tokio::test]
    async fn capture_and_release_target_the_hold() {
        let ok = |b: &str| Ok(HttpResponse { status: 200, body: b.into() });
        let t = MockTransport::scripted(
            vec![
                ok(r#"{"success":true,"data":{"hold_id":"h1","status":"Captured","captured_micros":900}}"#),
                ok(r#"{"success":true,"data":{"hold_id":"h2","status":"Released","captured_micros":0}}"#),
            ],
            HttpResponse { status: 500, body: "{}".into() },
        );
        let c = MintHoldsClient::new("https://mint.test", "mk", &t);
        let cap = c.capture("h1", 900, "acct_seller", "cap-k", "INV-1").await.unwrap();
        assert_eq!(cap.captured_micros, 900);
        let rel = c.release("h2", "rel-k", "void").await.unwrap();
        assert_eq!(rel.status, "Released");
        let reqs = t.requests();
        assert_eq!(reqs[0].url, "https://mint.test/v1/holds/h1/capture");
        assert_eq!(reqs[1].url, "https://mint.test/v1/holds/h2/release");
        let body: Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(body["to"], "acct_seller");
    }
}

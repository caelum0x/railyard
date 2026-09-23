//! # abacus-mint-client — real HTTP client to the LIVE mint-server.
//!
//! Implements [`MintTransport`] over `reqwest`. Authenticates with `X-API-Key`,
//! sends `i64` `amount_micros`, and reuses a deterministic per-request `idem_base`
//! across the hold/capture/release triple with distinct `:hold`/`:capture`/
//! `:release` suffixes so retries collapse server-side. Maps `{success,error.code}`
//! bodies onto [`AbacusError`] via [`map_error`].
//!
//! It implements BOTH Mint enforcement paths:
//!
//! - the **direct holds** path — `POST /v1/holds`, `POST /v1/holds/:id/capture`,
//!   `POST /v1/holds/:id/release`; and
//! - the **allowance ceiling** path — `POST /v1/allowances/:id/spend` (hold-backed
//!   scheme → `spend_receipt.hold_id`) and `GET /v1/allowances/:id`.
//!
//! Before a capture settles to a provider `ait1…` payee, the payee is resolved to
//! its `acct_…` via `GET /v1/addresses/:address` (the aitp-client gap-#1 bridge).
//!
//! Neither the base URL nor the API key is ever a literal: production wiring reads
//! `MINT_SERVER_URL` / `MINT_API_KEY` from the environment and passes them here.

use async_trait::async_trait;
use serde::Deserialize;

use abacus_core::error::AbacusError;
use abacus_core::transport::{
    Allowance, AllowanceSpend, AllowanceSpendReceipt, Capture, CaptureReceipt, Hold, HoldReceipt,
    MintTransport, Release, ReleaseReceipt, SettlementProof,
};
use aitp_client::codec::{decode_receipt, HEADER_RECEIPT};

/// A configured client for one mint-server deployment.
#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

/// The `{success:false,error:{code,message}}` shape returned by mint-server.
#[derive(Debug, Deserialize)]
struct MintErrorBody {
    error: MintErrorInner,
}

#[derive(Debug, Deserialize)]
struct MintErrorInner {
    code: String,
    #[serde(default)]
    message: String,
}

impl MintClient {
    /// Build a client from a base URL (e.g. `https://mint.internal`) and the shared
    /// `X-API-Key` secret. Both come from config/env at startup, never literals.
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        MintClient {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Send a JSON POST and decode either the success `data` payload or map the
    /// error body/status onto an [`AbacusError`].
    async fn post_json<B: serde::Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, AbacusError> {
        let resp = self
            .http
            .post(self.url(path))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;
        Self::decode(resp).await
    }

    /// POST a terminal hold operation and require Mint's signed receipt header.
    async fn post_json_receipted<B: serde::Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(T, SettlementProof), AbacusError> {
        let resp = self
            .http
            .post(self.url(path))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;
        let encoded = resp
            .headers()
            .get(HEADER_RECEIPT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let data = Self::decode(resp).await?;
        let encoded = encoded.ok_or_else(|| {
            AbacusError::Transport("Mint terminal response missing AITP-Receipt".to_string())
        })?;
        let receipt = decode_receipt(&encoded)
            .map_err(|error| AbacusError::Transport(format!("decode Mint receipt: {error}")))?;
        Ok((
            data,
            SettlementProof {
                sequence: receipt.seq,
                operation: receipt.op,
                operation_hash: receipt.op_hash,
                receipt_hash: receipt.hash,
                signature: receipt.sig,
            },
        ))
    }

    /// Send a JSON GET and decode the success `data` payload or map the error.
    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, AbacusError> {
        let resp = self
            .http
            .get(self.url(path))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;
        Self::decode(resp).await
    }

    async fn decode<T: for<'de> Deserialize<'de>>(
        resp: reqwest::Response,
    ) -> Result<T, AbacusError> {
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;

        if (200..300).contains(&status) {
            // Success envelope: {"success":true,"data":<T>}. Fall back to the raw
            // value if a route returns an un-enveloped body.
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| AbacusError::Transport(format!("decode success: {e}")))?;
            let data = value.get("data").cloned().unwrap_or(value);
            return serde_json::from_value(data)
                .map_err(|e| AbacusError::Transport(format!("decode data: {e}")));
        }

        // Error path: map by status first, then by stable code.
        let (code, message) = serde_json::from_str::<MintErrorBody>(&text)
            .map(|b| (b.error.code, b.error.message))
            .unwrap_or_else(|_| (String::new(), text.clone()));
        Err(map_error(status, &code, message))
    }

    /// Resolve a provider `ait1…` payee into its `acct_…` via
    /// `GET /v1/addresses/:address` (the account a capture's `to` requires).
    pub async fn resolve_payee_account(&self, address: &str) -> Result<String, AbacusError> {
        let data: AddressData = self.get_json(&format!("/v1/addresses/{address}")).await?;
        Ok(data.account_id)
    }
}

/// `GET /v1/addresses/:address` → `data`.
#[derive(Debug, Deserialize)]
struct AddressData {
    account_id: String,
}

/// Map a mint-server error `(status, code, message)` onto an [`AbacusError`].
/// Mirrors mint-core's stable codes so the enforcement point speaks the same
/// language as the ledger.
fn map_error(status: u16, code: &str, message: String) -> AbacusError {
    match (status, code) {
        (_, "allowance_exceeded") => AbacusError::AllowanceExceeded,
        (_, "insufficient_funds") => AbacusError::InsufficientFunds,
        (_, "account_frozen") => AbacusError::AccountFrozen,
        (_, "account_not_found") => AbacusError::AccountNotFound,
        (_, "hold_not_found") => AbacusError::HoldNotFound,
        (_, "allowance_not_found") => AbacusError::AllowanceNotFound,
        (_, "idempotency_conflict") => AbacusError::IdempotencyConflict,
        (_, "capture_exceeds_hold") => AbacusError::CaptureExceedsHold,
        (_, "payout_limit") | (_, "payout_limit_exceeded") => AbacusError::PayoutLimitExceeded,
        (402, _) => AbacusError::InsufficientFunds,
        (403, _) => AbacusError::AccountFrozen,
        (404, _) => AbacusError::AccountNotFound,
        (409, _) => AbacusError::IdempotencyConflict,
        (422, _) => AbacusError::CaptureExceedsHold,
        (429, _) => AbacusError::PayoutLimitExceeded,
        (401, _) => AbacusError::Unauthorized,
        _ => AbacusError::Upstream {
            status,
            code: code.to_string(),
            message,
        },
    }
}

// ---- wire request shapes (mint-server dto) --------------------------------

#[derive(serde::Serialize)]
struct HoldReq<'a> {
    from: &'a str,
    amount_micros: i64,
    idem_key: String,
}

#[derive(serde::Serialize)]
struct CaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: String,
}

#[derive(serde::Serialize)]
struct ReleaseReq {
    idem_key: String,
}

#[derive(serde::Serialize)]
struct AllowanceSpendReq<'a> {
    amount_micros: i64,
    to: &'a str,
    // Hold-backed scheme: the spend reserves a hold the caller later captures.
    scheme: &'static str,
    idem_key: String,
}

// ---- wire response shapes (mint-server views) -----------------------------

#[derive(Debug, Default, Deserialize)]
struct HoldData {
    #[serde(default)]
    hold_id: String,
}

#[derive(Debug, Default, Deserialize)]
struct CaptureData {
    #[serde(default)]
    captured_micros: i64,
    #[serde(default)]
    holder_balance: BalanceView,
}

#[derive(Debug, Default, Deserialize)]
struct BalanceView {
    #[serde(default)]
    available_micros: i64,
    #[serde(default)]
    updated_seq: u64,
}

#[derive(Debug, Default, Deserialize)]
struct ReleaseData {
    #[serde(default)]
    released_micros: i64,
}

#[derive(Debug, Default, Deserialize)]
struct AllowanceView {
    #[serde(default)]
    limit_micros: i64,
    #[serde(default)]
    spent_micros: i64,
    #[serde(default)]
    revoked: bool,
    #[serde(default)]
    id: String,
}

#[derive(Debug, Default, Deserialize)]
struct SpendReceiptData {
    #[serde(default)]
    hold_id: Option<String>,
    #[serde(default)]
    remaining_micros: i64,
    #[serde(default)]
    settlement_seq: u64,
}

#[async_trait]
impl MintTransport for MintClient {
    async fn ready(&self) -> Result<(), AbacusError> {
        // Mint's own readiness probe round-trips its ledger store; a 503 there
        // means holds cannot be opened or settled.
        let resp = self
            .http
            .get(self.url("/readyz"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(AbacusError::Transport(format!(
                "mint readyz returned {}",
                resp.status()
            )))
        }
    }

    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, AbacusError> {
        let data: HoldData = self
            .post_json(
                "/v1/holds",
                &HoldReq {
                    from: &req.account_id,
                    amount_micros: req.amount_micros,
                    idem_key: format!("{}:hold", req.idem_key),
                },
            )
            .await?;
        Ok(HoldReceipt {
            hold_id: data.hold_id,
            // Mint's hold view returns state, not the original requested
            // amount. The request is authoritative for the reservation size.
            amount_micros: req.amount_micros,
        })
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        req: Capture,
    ) -> Result<CaptureReceipt, AbacusError> {
        // Bridge the aitp-client gap #1: resolve the provider ait1… payee to the
        // acct_… the mint's `to` requires, immediately before capturing.
        let to = self.resolve_payee_account(&req.payee).await?;
        let (data, proof): (CaptureData, SettlementProof) = self
            .post_json_receipted(
                &format!("/v1/holds/{hold_id}/capture"),
                &CaptureReq {
                    amount_micros: req.amount_micros,
                    to: &to,
                    idem_key: format!("{}:capture", req.idem_key),
                },
            )
            .await?;
        if proof.operation != "hold.capture"
            || proof.sequence != data.holder_balance.updated_seq
            || proof.sequence == 0
        {
            return Err(AbacusError::Transport(
                "Mint capture receipt does not match capture response".to_string(),
            ));
        }
        Ok(CaptureReceipt {
            captured_micros: data.captured_micros,
            balance_after_micros: data.holder_balance.available_micros,
            settlement_seq: proof.sequence,
            proof,
        })
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        req: Release,
    ) -> Result<ReleaseReceipt, AbacusError> {
        let (data, proof): (ReleaseData, SettlementProof) = self
            .post_json_receipted(
                &format!("/v1/holds/{hold_id}/release"),
                &ReleaseReq {
                    idem_key: format!("{}:release", req.idem_key),
                },
            )
            .await?;
        if proof.operation != "hold.release" || proof.sequence == 0 {
            return Err(AbacusError::Transport(
                "Mint release receipt does not match release response".to_string(),
            ));
        }
        Ok(ReleaseReceipt {
            released_micros: data.released_micros,
            proof,
        })
    }

    async fn get_allowance(&self, allowance_id: &str) -> Result<Allowance, AbacusError> {
        let data: AllowanceView = self
            .get_json(&format!("/v1/allowances/{allowance_id}"))
            .await?;
        Ok(Allowance {
            allowance_id: if data.id.is_empty() {
                allowance_id.to_string()
            } else {
                data.id
            },
            limit_micros: data.limit_micros,
            spent_micros: data.spent_micros,
            revoked: data.revoked,
        })
    }

    async fn spend_allowance(
        &self,
        req: AllowanceSpend,
    ) -> Result<AllowanceSpendReceipt, AbacusError> {
        // Resolve the provider ait1… payee to the acct_… the spend settles to.
        let to = self.resolve_payee_account(&req.payee).await?;
        let data: SpendReceiptData = self
            .post_json(
                &format!("/v1/allowances/{}/spend", req.allowance_id),
                &AllowanceSpendReq {
                    amount_micros: req.amount_micros,
                    to: &to,
                    scheme: "hold",
                    idem_key: format!("{}:hold", req.idem_key),
                },
            )
            .await?;
        Ok(AllowanceSpendReceipt {
            hold_id: data.hold_id,
            remaining_micros: data.remaining_micros,
            settlement_seq: data.settlement_seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codes_to_enforcement_errors() {
        assert_eq!(
            map_error(402, "allowance_exceeded", String::new()),
            AbacusError::AllowanceExceeded
        );
        assert_eq!(
            map_error(402, "insufficient_funds", String::new()),
            AbacusError::InsufficientFunds
        );
        assert_eq!(
            map_error(403, "account_frozen", String::new()),
            AbacusError::AccountFrozen
        );
        assert_eq!(
            map_error(409, "x", String::new()),
            AbacusError::IdempotencyConflict
        );
        assert_eq!(
            map_error(422, "x", String::new()),
            AbacusError::CaptureExceedsHold
        );
        assert_eq!(
            map_error(429, "payout_limit", String::new()),
            AbacusError::PayoutLimitExceeded
        );
    }

    #[test]
    fn unmapped_status_is_upstream() {
        assert_eq!(
            map_error(500, "boom", "kaboom".into()),
            AbacusError::Upstream {
                status: 500,
                code: "boom".into(),
                message: "kaboom".into(),
            }
        );
    }

    #[test]
    fn hold_wire_uses_mints_from_field_and_preserves_reservation() {
        let body = serde_json::to_value(HoldReq {
            from: "acct_treasury",
            amount_micros: 4_200,
            idem_key: "request:hold".to_string(),
        })
        .unwrap();
        assert_eq!(body["from"], "acct_treasury");
        assert!(body.get("account_id").is_none());
        assert_eq!(body["amount_micros"], 4_200);
    }
}

//! Real HTTP client to Mint, implementing `MintMeter` and `RoyaltyInitiator`.
//!
//! Meters a passport-conditioned generation on the Mint ledger, tagging the
//! settlement with the passport id + version so Mint receipts and Ledgerline
//! margins are attributable per character/brand/persona. Success-only (ADR-0002);
//! the reference is the generation id, making the call idempotent on retry.
//!
//! Also initiates AITP-402 royalty transfers to a passport owner's Mint
//! account on third-party generate-AS (owner-set `Rights::royalty_bps`,
//! default 0), over Mint's real generic `POST /v1/transfers` primitive
//! (`apps/mint/crates/mint-server/src/handlers/transfers.rs`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use passport_core::{
    MintMeter, MintMeterRequest, MintMeterResult, PassportError, RoyaltyInitiator,
    RoyaltyTransferRequest, RoyaltyTransferResult,
};

/// A Mint ledger receipt reference — the opaque identifier that joins a
/// generation's identity side to its money side (ARCHITECTURE.md §6.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintReceiptRef(pub String);

impl MintReceiptRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// The real wire shape Mint's generic `POST /v1/transfers` accepts
/// (`mint-server::dto::TransferReq`). Used here to carry an AITP-402 royalty
/// payout from a generation's payer to the passport owner; exposed publicly
/// so a caller that already holds a [`MintClient`] can drive a bespoke
/// transfer without going through the [`RoyaltyInitiator`] port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Aitp402Transfer {
    pub from: String,
    pub to: String,
    pub amount_micros: i64,
    pub idem_key: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    path: String,
    transfer_path: String,
}

impl MintClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            path: "/v1/meter".to_string(),
            transfer_path: "/v1/transfers".to_string(),
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Override the path royalty transfers are POSTed to (defaults to Mint's
    /// real generic `/v1/transfers` endpoint).
    pub fn with_transfer_path(mut self, path: impl Into<String>) -> Self {
        self.transfer_path = path.into();
        self
    }

    /// Submit a real AITP-402 royalty payout on Mint's ledger.
    pub async fn royalty_transfer(
        &self,
        transfer: &Aitp402Transfer,
    ) -> Result<MintReceiptRef, PassportError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, self.transfer_path))
            .header("X-API-Key", &self.api_key)
            .json(transfer)
            .send()
            .await
            .map_err(|e| PassportError::Mint(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PassportError::Mint(format!(
                "mint royalty transfer returned {status}: {body}"
            )));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| PassportError::Mint(format!("decoding mint transfer response: {e}")))?;
        let receipt_hash = body
            .get("data")
            .and_then(|data| data.get("receipt"))
            .and_then(|receipt| receipt.get("hash"))
            .and_then(|hash| hash.as_str())
            .ok_or_else(|| {
                PassportError::Mint("mint transfer response is missing receipt.hash".into())
            })?;
        Ok(MintReceiptRef(receipt_hash.to_string()))
    }
}

#[async_trait]
impl MintMeter for MintClient {
    async fn meter(&self, request: &MintMeterRequest) -> Result<MintMeterResult, PassportError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, self.path))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| PassportError::Mint(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PassportError::Mint(format!(
                "mint returned {status}: {body}"
            )));
        }

        response
            .json::<MintMeterResult>()
            .await
            .map_err(|e| PassportError::Mint(format!("decoding mint response: {e}")))
    }
}

#[async_trait]
impl RoyaltyInitiator for MintClient {
    async fn transfer_royalty(
        &self,
        request: &RoyaltyTransferRequest,
    ) -> Result<RoyaltyTransferResult, PassportError> {
        if request.amount_micros <= 0 {
            return Ok(RoyaltyTransferResult { receipt_ref: None });
        }
        let transfer = Aitp402Transfer {
            from: request.payer_account.clone(),
            to: request.owner_account.clone(),
            amount_micros: request.amount_micros,
            // Idempotent on the generation id: a retried royalty attempt for
            // the same generation replays Mint's existing transfer rather
            // than double-paying the owner.
            idem_key: format!("passport_royalty_{}", request.reference),
            reference: Some(format!(
                "passport:{}:v{}:{}",
                request.passport_id, request.version, request.reference
            )),
        };
        let receipt = self.royalty_transfer(&transfer).await?;
        Ok(RoyaltyTransferResult {
            receipt_ref: Some(receipt.into_string()),
        })
    }
}

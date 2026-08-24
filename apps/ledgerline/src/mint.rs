//! Injectable Mint settlement seam and its real HTTP implementation.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::domain::{EventOutcome, MeterEvent, SettlementRefs};
use crate::error::LedgerlineError;

#[async_trait]
pub trait MintSettlement: Send + Sync {
    /// Settle the revenue hold/capture and provider COGS transfer for one event.
    async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError>;
}

/// Real rustls HTTP client for the Mint service.
#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

impl MintClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LedgerlineError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, path))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| LedgerlineError::Mint(error.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| LedgerlineError::Mint(error.to_string()))?;
        if !status.is_success() {
            return Err(LedgerlineError::Mint(format!(
                "Mint returned {status}: {text}"
            )));
        }
        let envelope: SuccessEnvelope<T> = serde_json::from_str(&text)
            .map_err(|error| LedgerlineError::Mint(format!("invalid Mint response: {error}")))?;
        Ok(envelope.data)
    }
}

#[derive(Deserialize)]
struct SuccessEnvelope<T> {
    data: T,
}

#[derive(Serialize)]
struct HoldCreate<'a> {
    from: &'a str,
    amount_micros: i64,
    idem_key: String,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Serialize)]
struct HoldCapture<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: String,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Serialize)]
struct Transfer<'a> {
    from: &'a str,
    to: &'a str,
    amount_micros: i64,
    idem_key: String,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Deserialize)]
struct HoldReceipt {
    hold_id: String,
    first_seq: i64,
}

#[derive(Deserialize)]
struct TransferReceipt {
    first_seq: i64,
}

#[async_trait]
impl MintSettlement for MintClient {
    async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError> {
        if event.outcome == EventOutcome::Failed {
            return Ok(SettlementRefs::failed_request());
        }

        // Each operation gets its own deterministic idempotency key. A retry
        // after a process crash safely resumes at whichever Mint call completed.
        // `request_id` is the financial identity. Even if a buggy caller sends
        // two different ingest keys concurrently, Mint still sees one set of
        // operation keys and cannot charge the request twice.
        let prefix = format!("ledgerline:{}", event.request_id);
        let hold: HoldReceipt = self
            .post(
                "/v1/holds",
                &HoldCreate {
                    from: &event.customer_account,
                    amount_micros: event.revenue_micros,
                    idem_key: format!("{prefix}:revenue:hold"),
                    ref_: &event.request_id,
                },
            )
            .await?;
        let capture: HoldReceipt = self
            .post(
                &format!("/v1/holds/{}/capture", hold.hold_id),
                &HoldCapture {
                    amount_micros: event.revenue_micros,
                    to: &event.revenue_account,
                    idem_key: format!("{prefix}:revenue:capture"),
                    ref_: &event.request_id,
                },
            )
            .await?;
        let cogs: TransferReceipt = self
            .post(
                "/v1/transfers",
                &Transfer {
                    from: &event.cogs_account,
                    to: &event.provider_account,
                    amount_micros: event.cogs_micros,
                    idem_key: format!("{prefix}:cogs:transfer"),
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(SettlementRefs {
            revenue_hold_id: Some(hold.hold_id),
            revenue_sequence: Some(capture.first_seq),
            cogs_sequence: Some(cogs.first_seq),
        })
    }
}

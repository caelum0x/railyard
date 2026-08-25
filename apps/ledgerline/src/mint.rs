//! Injectable Mint settlement seams and the real HTTP implementation.
//!
//! Two seams live here:
//!
//! * [`MintSettlement`] — the high-level "settle one event" port consumed by the
//!   server. The durable coordinator in [`crate::settle`] implements it.
//! * [`MintOps`] — the low-level, individually idempotent Mint primitives (hold,
//!   capture, COGS transfer) plus their compensating inverses. The coordinator
//!   drives these to make the hold+capture+COGS trio atomic-or-compensated.
//!
//! [`MintClient`] speaks real Mint HTTP and implements [`MintOps`]. Every call
//! carries a caller-supplied `idem_key`, so replaying a primitive is a no-op on
//! Mint's side; that is the property the coordinator relies on for crash-safe
//! resume and idempotent retry.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::domain::{MeterEvent, SettlementRefs};
use crate::error::LedgerlineError;

/// High-level settlement port: settle one event's money movements as a unit.
#[async_trait]
pub trait MintSettlement: Send + Sync {
    /// Settle the revenue hold/capture and provider COGS transfer for one event.
    ///
    /// Implementations MUST be atomic-or-compensated: on success the whole trio
    /// committed; on failure any partial effects were reversed. Callers therefore
    /// never observe a half-settled request.
    async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError>;
}

/// Reference returned by a successful hold creation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldRef {
    pub hold_id: String,
    pub first_seq: i64,
}

/// Low-level, individually idempotent Mint primitives and their compensations.
///
/// Forward operations move money; compensating operations reverse the exact
/// effect of their forward counterpart. All calls are keyed by `idem_key` so a
/// retry with the same key is a no-op, and every method returns `Result` so the
/// coordinator can decide between resume and compensation.
#[async_trait]
pub trait MintOps: Send + Sync {
    /// Place a revenue hold on the customer account.
    async fn create_hold(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<HoldRef, LedgerlineError>;

    /// Capture a previously created hold into the revenue account.
    async fn capture_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError>;

    /// Transfer provider COGS from the COGS account to the provider account.
    async fn transfer_cogs(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError>;

    /// Compensation for [`create_hold`](Self::create_hold): release an
    /// uncaptured hold, returning the escrowed funds to the customer.
    async fn release_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError>;

    /// Compensation for [`capture_hold`](Self::capture_hold): move the captured
    /// revenue back from the revenue account to the customer account.
    async fn reverse_revenue(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError>;

    /// Compensation for [`transfer_cogs`](Self::transfer_cogs): move COGS back
    /// from the provider account to the COGS account.
    async fn reverse_cogs(&self, event: &MeterEvent, idem_key: &str)
        -> Result<(), LedgerlineError>;
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
    idem_key: &'a str,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Serialize)]
struct HoldCapture<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Serialize)]
struct HoldRelease<'a> {
    idem_key: &'a str,
    #[serde(rename = "ref")]
    ref_: &'a str,
}

#[derive(Serialize)]
struct Transfer<'a> {
    from: &'a str,
    to: &'a str,
    amount_micros: i64,
    idem_key: &'a str,
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
impl MintOps for MintClient {
    async fn create_hold(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<HoldRef, LedgerlineError> {
        let hold: HoldReceipt = self
            .post(
                "/v1/holds",
                &HoldCreate {
                    from: &event.customer_account,
                    amount_micros: event.revenue_micros,
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(HoldRef {
            hold_id: hold.hold_id,
            first_seq: hold.first_seq,
        })
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError> {
        let capture: HoldReceipt = self
            .post(
                &format!("/v1/holds/{hold_id}/capture"),
                &HoldCapture {
                    amount_micros: event.revenue_micros,
                    to: &event.revenue_account,
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(capture.first_seq)
    }

    async fn transfer_cogs(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError> {
        let cogs: TransferReceipt = self
            .post(
                "/v1/transfers",
                &Transfer {
                    from: &event.cogs_account,
                    to: &event.provider_account,
                    amount_micros: event.cogs_micros,
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(cogs.first_seq)
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        let _: HoldReceipt = self
            .post(
                &format!("/v1/holds/{hold_id}/release"),
                &HoldRelease {
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(())
    }

    async fn reverse_revenue(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        // Undo the capture by moving revenue back to the customer account.
        let _: TransferReceipt = self
            .post(
                "/v1/transfers",
                &Transfer {
                    from: &event.revenue_account,
                    to: &event.customer_account,
                    amount_micros: event.revenue_micros,
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(())
    }

    async fn reverse_cogs(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        // Undo the COGS transfer by moving it back from provider to COGS.
        let _: TransferReceipt = self
            .post(
                "/v1/transfers",
                &Transfer {
                    from: &event.provider_account,
                    to: &event.cogs_account,
                    amount_micros: event.cogs_micros,
                    idem_key,
                    ref_: &event.request_id,
                },
            )
            .await?;
        Ok(())
    }
}

//! Pure Ledgerline domain types and integer-microdollar invariants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::LedgerlineError;

/// Where the event's COGS measurement came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// Relay supplied an exact settled provider cost.
    RelayGateway,
    /// A customer supplied the event; COGS can be an estimate until true-up.
    Sdk,
    /// ReelForge emitted the event while dogfooding Ledgerline.
    Reelforge,
}

/// Whether the underlying AI request delivered a billable result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventOutcome {
    Succeeded,
    Failed,
}

/// Signed settlement evidence emitted by Relay/Mint for the source request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSettlementReceipt {
    pub request_id: String,
    pub payer_account: String,
    pub sequence: u64,
    pub operation: String,
    pub amount_micros: i64,
    pub operation_hash: String,
    pub receipt_hash: String,
    pub signature: String,
}

/// One request-level usage event. Money is always integer microdollars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterEvent {
    /// Caller-generated replay key. Reusing it with different data is rejected.
    pub idempotency_key: String,
    /// Stable request correlation key shared with Relay/Mint receipts.
    pub request_id: String,
    pub customer_id: String,
    pub feature: String,
    pub model: String,
    pub source: EventSource,
    pub outcome: EventOutcome,
    /// Customer price for this request, in integer microdollars.
    pub revenue_micros: i64,
    /// Exact or estimated provider cost, in integer microdollars.
    pub cogs_micros: i64,
    /// Mint account funding the customer-facing revenue hold.
    pub customer_account: String,
    /// Mint account receiving captured revenue.
    pub revenue_account: String,
    /// Mint account funding provider COGS.
    pub cogs_account: String,
    /// Mint account receiving provider COGS.
    pub provider_account: String,
    /// Relay/Mint proof correlating this margin event to the settled request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_settlement_receipt: Option<SourceSettlementReceipt>,
}

impl MeterEvent {
    /// Enforce Ledgerline's financial boundary before any Mint call is made.
    pub fn validate(&self) -> Result<(), LedgerlineError> {
        for (name, value) in [
            ("idempotency_key", self.idempotency_key.as_str()),
            ("request_id", self.request_id.as_str()),
            ("customer_id", self.customer_id.as_str()),
            ("feature", self.feature.as_str()),
            ("model", self.model.as_str()),
            ("customer_account", self.customer_account.as_str()),
            ("revenue_account", self.revenue_account.as_str()),
            ("cogs_account", self.cogs_account.as_str()),
            ("provider_account", self.provider_account.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(LedgerlineError::Invalid(format!(
                    "{name} must not be empty"
                )));
            }
        }

        match self.outcome {
            EventOutcome::Succeeded => {
                if self.revenue_micros <= 0 || self.cogs_micros <= 0 {
                    return Err(LedgerlineError::Invalid(
                        "successful events require positive revenue_micros and cogs_micros"
                            .to_string(),
                    ));
                }
            }
            EventOutcome::Failed => {
                if self.revenue_micros != 0 || self.cogs_micros != 0 {
                    return Err(LedgerlineError::Invalid(
                        "failed events must have zero revenue_micros and cogs_micros".to_string(),
                    ));
                }
            }
        }

        if let Some(receipt) = &self.source_settlement_receipt {
            if receipt.request_id != self.request_id
                || receipt.payer_account != self.customer_account
                || receipt.sequence == 0
                || receipt.operation_hash.is_empty()
                || receipt.receipt_hash.is_empty()
                || receipt.signature.is_empty()
            {
                return Err(LedgerlineError::Invalid(
                    "source settlement receipt does not match request principal/correlation"
                        .to_string(),
                ));
            }
            match self.outcome {
                EventOutcome::Succeeded
                    if receipt.operation != "hold.capture"
                        || receipt.amount_micros != self.cogs_micros =>
                {
                    return Err(LedgerlineError::Invalid(
                        "successful source receipt must capture the exact COGS amount".to_string(),
                    ));
                }
                EventOutcome::Failed if receipt.operation != "hold.release" => {
                    return Err(LedgerlineError::Invalid(
                        "failed source receipt must prove hold release".to_string(),
                    ));
                }
                _ => {}
            }
        } else if self.source == EventSource::RelayGateway {
            return Err(LedgerlineError::Invalid(
                "Relay events require signed source settlement evidence".to_string(),
            ));
        }
        Ok(())
    }
}

/// Mint references proving both sides of a successful request settled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRefs {
    pub revenue_hold_id: Option<String>,
    pub revenue_sequence: Option<i64>,
    pub cogs_sequence: Option<i64>,
}

impl SettlementRefs {
    pub const fn failed_request() -> Self {
        Self {
            revenue_hold_id: None,
            revenue_sequence: None,
            cogs_sequence: None,
        }
    }
}

/// The request-level product record returned by ingest and margin APIs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarginRecord {
    #[serde(flatten)]
    pub event: MeterEvent,
    pub margin_micros: i64,
    /// Gross margin in basis points. `None` when revenue is zero.
    pub margin_bps: Option<i64>,
    pub settlement: SettlementRefs,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub idempotent_replay: bool,
}

impl MarginRecord {
    pub fn new(
        event: MeterEvent,
        settlement: SettlementRefs,
        created_at: DateTime<Utc>,
    ) -> Result<Self, LedgerlineError> {
        let margin_micros = event
            .revenue_micros
            .checked_sub(event.cogs_micros)
            .ok_or(LedgerlineError::Overflow)?;
        let margin_bps = if event.revenue_micros == 0 {
            None
        } else {
            let scaled = i128::from(margin_micros) * 10_000;
            Some(
                i64::try_from(scaled / i128::from(event.revenue_micros))
                    .map_err(|_| LedgerlineError::Overflow)?,
            )
        };
        Ok(Self {
            event,
            margin_micros,
            margin_bps,
            settlement,
            created_at,
            idempotent_replay: false,
        })
    }
}

/// Optional dimensions for the margin explorer endpoint.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MarginFilter {
    pub customer_id: Option<String>,
    pub feature: Option<String>,
    pub model: Option<String>,
    pub limit: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> MeterEvent {
        MeterEvent {
            idempotency_key: "evt-1".into(),
            request_id: "req-1".into(),
            customer_id: "customer-1".into(),
            feature: "storyboard".into(),
            model: "openai/gpt-5".into(),
            source: EventSource::RelayGateway,
            outcome: EventOutcome::Succeeded,
            revenue_micros: 1_000_000,
            cogs_micros: 1_250_000,
            customer_account: "acct_customer".into(),
            revenue_account: "acct_revenue".into(),
            cogs_account: "acct_cogs".into(),
            provider_account: "acct_provider".into(),
            source_settlement_receipt: Some(SourceSettlementReceipt {
                request_id: "req-1".into(),
                payer_account: "acct_customer".into(),
                sequence: 42,
                operation: "hold.capture".into(),
                amount_micros: 1_250_000,
                operation_hash: "op_hash".into(),
                receipt_hash: "receipt_hash".into(),
                signature: "signature".into(),
            }),
        }
    }

    #[test]
    fn negative_margin_is_exact_integer_math() {
        let record = MarginRecord::new(event(), SettlementRefs::failed_request(), Utc::now())
            .expect("valid record");
        assert_eq!(record.margin_micros, -250_000);
        assert_eq!(record.margin_bps, Some(-2_500));
    }

    #[test]
    fn consumes_the_shared_settlement_receipt_vocabulary() {
        let receipt: SourceSettlementReceipt = serde_json::from_value(serde_json::json!({
            "request_id": "gateway_job_1",
            "payer_account": "acct_owner",
            "sequence": 7,
            "operation": "hold.capture",
            "amount_micros": 42,
            "operation_hash": "op_hash",
            "receipt_hash": "receipt_hash",
            "signature": "signature"
        }))
        .unwrap();
        assert_eq!(receipt.request_id, "gateway_job_1");
        assert_eq!(receipt.amount_micros, 42);
    }

    #[test]
    fn failed_requests_must_be_free() {
        let mut input = event();
        input.outcome = EventOutcome::Failed;
        assert!(input.validate().is_err());
        input.revenue_micros = 0;
        input.cogs_micros = 0;
        let receipt = input.source_settlement_receipt.as_mut().unwrap();
        receipt.operation = "hold.release".into();
        input.validate().expect("zero-cost failure is valid");
    }

    #[test]
    fn relay_receipt_is_principal_and_request_correlated() {
        let mut input = event();
        input.source_settlement_receipt.as_mut().unwrap().request_id = "another-request".into();
        assert!(input.validate().is_err());

        let mut missing = event();
        missing.source_settlement_receipt = None;
        assert!(missing.validate().is_err());
    }
}

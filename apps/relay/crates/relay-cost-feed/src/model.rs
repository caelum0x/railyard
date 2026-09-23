use relay_contract::{Microdollars, Usage};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

use crate::COST_EVENT_SCHEMA_VERSION;

/// Receipt-hash prefix marking meter-only (postpaid, unsettled) usage events.
pub const METERED_RECEIPT_PREFIX: &str = "metered:";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum InferenceRoute {
    #[default]
    Unknown,
    NativeInference,
    OpenAiChatCompletions,
    OpenAiEmbeddings,
    BatchInference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CostEvent {
    pub schema_version: u16,
    pub event_id: Uuid,
    pub request_id: Uuid,
    pub owner_principal: String,
    pub model: String,
    pub route: InferenceRoute,
    pub usage: Usage,
    pub authorized_micros: Microdollars,
    pub charged_micros: Microdollars,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider_cost_micros: Option<Microdollars>,
    pub overage_capped: bool,
    pub receipt_hash: String,
    pub settled_at_ms: i64,
}

impl CostEvent {
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor mirrors the immutable captured-settlement event contract"
    )]
    pub fn captured(
        request_id: Uuid,
        owner_principal: impl Into<String>,
        model: impl Into<String>,
        route: InferenceRoute,
        usage: Usage,
        authorized_micros: Microdollars,
        charged_micros: Microdollars,
        receipt_hash: impl Into<String>,
        settled_at_ms: i64,
    ) -> Self {
        Self {
            schema_version: COST_EVENT_SCHEMA_VERSION,
            // Relay has exactly one terminal charge per request. Reusing the
            // request UUID makes recovery/replay insertion idempotent.
            event_id: request_id,
            request_id,
            owner_principal: owner_principal.into(),
            model: model.into(),
            route,
            usage,
            authorized_micros,
            charged_micros,
            provider_cost_micros: usage.provider_cost_micros,
            overage_capped: charged_micros < usage.provider_cost_micros.unwrap_or(charged_micros),
            receipt_hash: receipt_hash.into(),
            settled_at_ms,
        }
    }

    /// Construct a METER-ONLY usage event for postpaid deployments.
    ///
    /// Meter-only mode records every successful inference priced at the
    /// catalog retail rate WITHOUT an AITP authorization or Mint hold, so the
    /// event is not backed by a signed settlement receipt. The receipt hash is
    /// therefore a deterministic `metered:<request_id>` marker (see
    /// [`CostEvent::is_metered`]), and the authorized amount equals the charge:
    /// downstream invoicing (e.g. Rago) is the system of record for payment.
    pub fn metered(
        request_id: Uuid,
        owner_principal: impl Into<String>,
        model: impl Into<String>,
        route: InferenceRoute,
        usage: Usage,
        charged_micros: Microdollars,
        settled_at_ms: i64,
    ) -> Self {
        Self {
            schema_version: COST_EVENT_SCHEMA_VERSION,
            event_id: request_id,
            request_id,
            owner_principal: owner_principal.into(),
            model: model.into(),
            route,
            usage,
            authorized_micros: charged_micros,
            charged_micros,
            provider_cost_micros: usage.provider_cost_micros,
            overage_capped: false,
            receipt_hash: format!("{METERED_RECEIPT_PREFIX}{request_id}"),
            settled_at_ms,
        }
    }

    /// True when the event was recorded by meter-only mode (no Mint receipt).
    pub fn is_metered(&self) -> bool {
        self.receipt_hash.starts_with(METERED_RECEIPT_PREFIX)
    }

    pub fn validate(&self) -> std::result::Result<(), CostEventValidationError> {
        if self.schema_version != COST_EVENT_SCHEMA_VERSION {
            return Err(CostEventValidationError::SchemaVersion);
        }
        if self.owner_principal.trim().is_empty()
            || self.model.trim().is_empty()
            || self.receipt_hash.trim().is_empty()
        {
            return Err(CostEventValidationError::Identity);
        }
        if self.authorized_micros.get() < 0
            || self.charged_micros.get() < 0
            || self.charged_micros > self.authorized_micros
        {
            return Err(CostEventValidationError::Money);
        }
        if self.usage.total_tokens
            < self
                .usage
                .input_tokens
                .saturating_add(self.usage.output_tokens)
        {
            return Err(CostEventValidationError::Usage);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CostEventValidationError {
    #[error("unsupported schema version")]
    SchemaVersion,
    #[error("principal, model, and receipt hash are required")]
    Identity,
    #[error("charge must be non-negative and cannot exceed authorization")]
    Money,
    #[error("total tokens cannot be smaller than input plus output")]
    Usage,
}

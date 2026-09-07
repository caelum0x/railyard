//! Relay JSON envelope types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::settlement::SettlementEvidence;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct DataEnvelope<T> {
    pub data: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiErrorEnvelope {
    pub error: ApiError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiError {
    pub message: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub param: Option<String>,
}

/// HTTP 409 returned when a paid retry names a request that already captured.
/// Relay cannot replay the provider body, so it returns the authenticated
/// terminal evidence and clients surface this as a recoverable typed outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SettledReplay {
    pub error: ApiError,
    pub data: SettlementEvidence,
}

impl ApiErrorEnvelope {
    pub fn message(&self) -> &str {
        &self.error.message
    }
}

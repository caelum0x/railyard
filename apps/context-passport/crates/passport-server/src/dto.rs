//! Request bodies specific to the HTTP surface.

use serde::Deserialize;

use passport_core::PaymentEvidence;
use passport_resolve::Modality;

#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    pub provider: String,
    pub modality: Modality,
    #[serde(default)]
    pub version: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    pub provider: String,
    pub modality: Modality,
    #[serde(default)]
    pub version: Option<i64>,
}

/// Record a generation executed by another first-party surface (for example
/// Menivor/Prism) against the exact locked passport version it resolved.
#[derive(Debug, Deserialize)]
pub struct AttestRequest {
    pub provider: String,
    pub modality: Modality,
    #[serde(default)]
    pub version: Option<i64>,
    pub output_ref: String,
    #[serde(default)]
    pub asset_hash: Option<String>,
    pub payment_evidence: PaymentEvidence,
    pub settled_at_ms: i64,
}

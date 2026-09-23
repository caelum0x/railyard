//! Request bodies specific to the HTTP surface.

use serde::Deserialize;

use passport_core::{ArtifactKind, ArtifactStatus, PaymentEvidence};
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
    /// What to produce as this passport (e.g. "Write a launch tweet about...").
    #[serde(default)]
    pub prompt: Option<String>,
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

/// `POST /v1/passports/:id/artifacts`
#[derive(Debug, Deserialize)]
pub struct RegisterArtifactRequest {
    pub provider: String,
    pub kind: ArtifactKind,
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub artifact_ref: Option<String>,
    #[serde(default)]
    pub provider_model: Option<String>,
    #[serde(default)]
    pub status: Option<ArtifactStatus>,
}

/// `PATCH /v1/passports/:id/artifacts/:artifact_id`
#[derive(Debug, Deserialize)]
pub struct UpdateArtifactRequest {
    pub status: ArtifactStatus,
    #[serde(default)]
    pub artifact_ref: Option<String>,
    #[serde(default)]
    pub provider_model: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

/// Optional body of `DELETE /v1/passports/:id/artifacts/:artifact_id`.
#[derive(Debug, Default, Deserialize)]
pub struct RevokeArtifactRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

//! The generated-asset provenance record.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use passport_core::{new_generation_id, DisclosureLevel, PassportVersion, PaymentEvidence};

/// One record per generated asset. `passport_content_digest` binds the asset to
/// the exact immutable identity snapshot that conditioned it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub id: String,
    pub passport_id: String,
    pub version: i64,
    /// Content address of the locked version that conditioned this asset.
    #[serde(default)]
    pub passport_content_digest: Option<String>,
    pub provider: String,
    pub modality: String,
    /// Reference to the produced asset (R2 key, URL, content hash).
    pub output_ref: String,
    /// Optional hard-binding hash of the produced asset (C2PA).
    #[serde(default)]
    pub asset_hash: Option<String>,
    /// IPTC digitalSourceType, frozen from the passport's rights at generation.
    pub iptc_digital_source_type: String,
    /// Whether synthetic-media disclosure was required at generation time.
    pub disclosure_required: bool,
    /// Optional Mint receipt reference proving the generation settled on the rail.
    #[serde(default)]
    pub mint_receipt_ref: Option<String>,
    /// Full request/principal-correlated signed evidence. Older stored records
    /// deserialize with `None`; new generation writes require `Some`.
    #[serde(default)]
    pub payment_evidence: Option<PaymentEvidence>,
    /// The caller's generation instruction (what was asked of the passport).
    #[serde(default)]
    pub prompt: Option<String>,
    /// The produced text for text-modality generations, so the asset itself is
    /// retrievable next to its provenance.
    #[serde(default)]
    pub output_text: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl GenerationRecord {
    /// Build a record from the locked version that conditioned the asset,
    /// freezing the rights-derived provenance fields.
    pub fn new(
        version: &PassportVersion,
        provider: impl Into<String>,
        modality: impl Into<String>,
        output_ref: impl Into<String>,
        asset_hash: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self::new_with_id(
            new_generation_id(),
            version,
            provider,
            modality,
            output_ref,
            asset_hash,
            created_at,
        )
    }

    /// Build a record with a caller-derived stable id. Used for external
    /// generation attestation where Relay's request id is the idempotency key.
    pub fn new_with_id(
        id: impl Into<String>,
        version: &PassportVersion,
        provider: impl Into<String>,
        modality: impl Into<String>,
        output_ref: impl Into<String>,
        asset_hash: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            passport_id: version.passport_id.clone(),
            version: version.version,
            passport_content_digest: version
                .content_digest
                .as_ref()
                .map(|d| d.as_str().to_string()),
            provider: provider.into(),
            modality: modality.into(),
            output_ref: output_ref.into(),
            asset_hash,
            iptc_digital_source_type: version.rights.iptc_digital_source_type.clone(),
            disclosure_required: matches!(version.rights.disclosure, DisclosureLevel::Required),
            mint_receipt_ref: None,
            payment_evidence: None,
            prompt: None,
            output_text: None,
            created_at,
        }
    }

    /// Attach the generation instruction and produced text (returns a new record).
    pub fn with_output(&self, prompt: Option<String>, output_text: Option<String>) -> Self {
        let mut next = self.clone();
        next.prompt = prompt;
        next.output_text = output_text;
        next
    }

    /// Attach a Mint receipt reference (returns a new record; no mutation).
    pub fn with_payment_evidence(&self, evidence: PaymentEvidence) -> Self {
        let mut next = self.clone();
        next.mint_receipt_ref = Some(evidence.receipt.receipt_hash.clone());
        next.payment_evidence = Some(evidence);
        next
    }
}

//! The generated-asset provenance record.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use passport_core::{new_generation_id, DisclosureLevel, PassportVersion};

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
        Self {
            id: new_generation_id(),
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
            created_at,
        }
    }

    /// Attach a Mint receipt reference (returns a new record; no mutation).
    pub fn with_receipt(&self, receipt_ref: Option<String>) -> Self {
        let mut next = self.clone();
        next.mint_receipt_ref = receipt_ref;
        next
    }
}

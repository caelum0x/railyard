//! Minimal C2PA-aligned manifest export.
//!
//! Emits the interop-critical constructs — the hard-binding asset hash, a
//! `c2pa.actions` assertion with `digitalSourceType` + `softwareAgent`, and
//! `ingredients` (the passport identity as an `inputTo` ingredient) — so a
//! downstream exporter can produce a real Content Credential. This is a faithful
//! subset, not a signed JUMBF manifest store (that is the Sx3 target).

use serde_json::{json, Value};

use crate::record::GenerationRecord;

/// Render a generation record as a C2PA-shaped manifest fragment.
pub fn to_c2pa_manifest(record: &GenerationRecord) -> Value {
    json!({
        "claim_generator": "railyard/context-passport",
        "assertions": [
            {
                "label": "c2pa.hash.data",
                "data": { "alg": "sha-256", "hash": record.asset_hash }
            },
            {
                "label": "c2pa.actions",
                "data": {
                    "actions": [{
                        "action": "c2pa.created",
                        "digitalSourceType": record.iptc_digital_source_type,
                        "softwareAgent": { "name": record.provider }
                    }]
                }
            }
        ],
        "ingredients": [{
            "title": "character-lock-passport",
            "relationship": "inputTo",
            "passport_id": record.passport_id,
            "passport_version": record.version,
            "passport_content_digest": record.passport_content_digest
        }],
        "metadata": {
            "modality": record.modality,
            "disclosure_required": record.disclosure_required,
            "mint_receipt_ref": record.mint_receipt_ref,
            "created_at": record.created_at
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn manifest_carries_source_type_and_passport_ingredient() {
        let record = GenerationRecord {
            id: "gen_1".into(),
            passport_id: "psp_1".into(),
            version: 3,
            passport_content_digest: Some("ni:///sha-256;abc".into()),
            provider: "heygen".into(),
            modality: "video".into(),
            output_ref: "r2://out.mp4".into(),
            asset_hash: Some("sha256:deadbeef".into()),
            iptc_digital_source_type:
                "http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia".into(),
            disclosure_required: true,
            mint_receipt_ref: Some("rcpt_9".into()),
            created_at: Utc::now(),
        };
        let m = to_c2pa_manifest(&record);
        assert_eq!(
            m["ingredients"][0]["passport_content_digest"],
            "ni:///sha-256;abc"
        );
        assert_eq!(
            m["assertions"][1]["data"]["actions"][0]["softwareAgent"]["name"],
            "heygen"
        );
    }
}

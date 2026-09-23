//! Locked, typed asset fields for a VOICE identity.
//!
//! A voice passport locks the SOURCE of a voice (reference recordings, language,
//! accent, delivery) plus the consent evidence for cloning it. The per-provider
//! clone a vendor mints from those samples (an ElevenLabs `voice_id`, a
//! Cartesia embedding id, ...) is NOT part of the locked identity: it is a
//! derived [`crate::TrainedArtifact`] registered per provider, because the same
//! voice has a different id on every vendor.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceAssets {
    /// Reference recording refs (R2 keys, content hashes, URLs) providers clone from.
    #[serde(default)]
    pub sample_refs: Vec<String>,
    /// Primary spoken language (BCP-47, e.g. `en-US`).
    #[serde(default)]
    pub language: Option<String>,
    /// Accent / dialect descriptor.
    #[serde(default)]
    pub accent: Option<String>,
    /// Delivery descriptor (timbre, pace, energy) woven into TTS directions.
    #[serde(default)]
    pub delivery: Option<String>,
    /// Hash reference to the recorded speaker consent for cloning this voice
    /// (the recording / signed release itself stays off-passport).
    #[serde(default)]
    pub consent_evidence_ref: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePatch {
    #[serde(default)]
    pub sample_refs: Option<Vec<String>>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub delivery: Option<String>,
    #[serde(default)]
    pub consent_evidence_ref: Option<String>,
}

impl VoiceAssets {
    /// Return a new value with the patch applied; never mutates in place.
    pub fn apply(&self, patch: &VoicePatch) -> Self {
        let mut next = self.clone();
        if let Some(v) = &patch.sample_refs {
            next.sample_refs = v.clone();
        }
        if let Some(v) = &patch.language {
            next.language = Some(v.clone());
        }
        if let Some(v) = &patch.accent {
            next.accent = Some(v.clone());
        }
        if let Some(v) = &patch.delivery {
            next.delivery = Some(v.clone());
        }
        if let Some(v) = &patch.consent_evidence_ref {
            next.consent_evidence_ref = Some(v.clone());
        }
        next
    }
}

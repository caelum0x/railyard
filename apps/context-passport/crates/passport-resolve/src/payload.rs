//! The provider-neutral conditioning payload a Relay adapter maps onto a vendor.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::modality::Modality;

/// A provider-agnostic conditioning payload. Only the fields relevant to the
/// requested modality are populated; a Relay adapter maps these onto a concrete
/// vendor request (Veo, Runway, Kling, HeyGen, ElevenLabs, a local LoRA, …).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditioningPayload {
    pub passport_id: String,
    pub version: i64,
    pub provider: String,
    pub modality: Modality,
    /// Reference images (faces, logo, watermark) to condition on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_images: Vec<String>,
    /// LoRA / fine-tune reference for consistent visual identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_ref: Option<String>,
    /// IP-Adapter / textual-inversion embedding reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_ref: Option<String>,
    /// Talking-head / avatar reference for video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_ref: Option<String>,
    /// Provider voice identity for voice/video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// Assembled positive style prompt fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_prompt: Option<String>,
    /// Assembled negative prompt (things to avoid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    /// Free-form text directives (tone, hooks, brand voice).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_directives: Vec<String>,
    /// Structured extras a vendor adapter may consume (palette, platform, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl ConditioningPayload {
    pub fn base(passport_id: &str, version: i64, provider: &str, modality: Modality) -> Self {
        Self {
            passport_id: passport_id.to_string(),
            version,
            provider: provider.to_string(),
            modality,
            ..Default::default()
        }
    }

    /// True when nothing usable was resolved for the requested modality.
    pub fn is_empty(&self) -> bool {
        self.reference_images.is_empty()
            && self.lora_ref.is_none()
            && self.embedding_ref.is_none()
            && self.avatar_ref.is_none()
            && self.voice_id.is_none()
            && self.style_prompt.is_none()
            && self.negative_prompt.is_none()
            && self.text_directives.is_empty()
            && self.extra.is_empty()
    }
}

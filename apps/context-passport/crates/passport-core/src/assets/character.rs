//! Locked, typed asset fields for a CHARACTER identity.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterAssets {
    /// Face / appearance reference image refs (R2 keys, content hashes, URLs).
    #[serde(default)]
    pub face_refs: Vec<String>,
    /// Provider voice identity (ElevenLabs / Cartesia / etc).
    #[serde(default)]
    pub voice_id: Option<String>,
    /// Rendered avatar / talking-head reference (HeyGen / Hedra).
    #[serde(default)]
    pub avatar_ref: Option<String>,
    /// LoRA / embedding reference for consistent generation.
    #[serde(default)]
    pub lora_ref: Option<String>,
    /// Visual style descriptor woven into prompts.
    #[serde(default)]
    pub style: Option<String>,
    /// Personality and tone-of-voice guidance.
    #[serde(default)]
    pub personality: Option<String>,
    /// Body / wardrobe descriptor.
    #[serde(default)]
    pub wardrobe: Option<String>,
}

/// A partial edit to a draft character's assets. `Option<Vec>` replaces the list
/// when present; scalar `Option`s set the value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterPatch {
    #[serde(default)]
    pub face_refs: Option<Vec<String>>,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub avatar_ref: Option<String>,
    #[serde(default)]
    pub lora_ref: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub wardrobe: Option<String>,
}

impl CharacterAssets {
    /// Return a new value with the patch applied; never mutates in place.
    pub fn apply(&self, patch: &CharacterPatch) -> Self {
        let mut next = self.clone();
        if let Some(v) = &patch.face_refs {
            next.face_refs = v.clone();
        }
        if let Some(v) = &patch.voice_id {
            next.voice_id = Some(v.clone());
        }
        if let Some(v) = &patch.avatar_ref {
            next.avatar_ref = Some(v.clone());
        }
        if let Some(v) = &patch.lora_ref {
            next.lora_ref = Some(v.clone());
        }
        if let Some(v) = &patch.style {
            next.style = Some(v.clone());
        }
        if let Some(v) = &patch.personality {
            next.personality = Some(v.clone());
        }
        if let Some(v) = &patch.wardrobe {
            next.wardrobe = Some(v.clone());
        }
        next
    }
}

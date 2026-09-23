//! Locked, typed asset fields for a visual STYLE identity (a house look that
//! any character or brand can be rendered in).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleAssets {
    /// Style reference image refs (moodboard frames, key art).
    #[serde(default)]
    pub style_refs: Vec<String>,
    /// Colour palette (hex or named colours).
    #[serde(default)]
    pub palette: Vec<String>,
    /// Positive style descriptor woven into prompts.
    #[serde(default)]
    pub descriptor: Option<String>,
    /// Medium (photography, 3d render, watercolor, anime, ...).
    #[serde(default)]
    pub medium: Option<String>,
    /// Things generations in this style must avoid.
    #[serde(default)]
    pub negative_prompt: Option<String>,
    /// Portable LoRA reference (a provider-specific trained LoRA is registered
    /// as a trained artifact instead).
    #[serde(default)]
    pub lora_ref: Option<String>,
    /// IP-Adapter / textual-inversion embedding reference.
    #[serde(default)]
    pub embedding_ref: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StylePatch {
    #[serde(default)]
    pub style_refs: Option<Vec<String>>,
    #[serde(default)]
    pub palette: Option<Vec<String>>,
    #[serde(default)]
    pub descriptor: Option<String>,
    #[serde(default)]
    pub medium: Option<String>,
    #[serde(default)]
    pub negative_prompt: Option<String>,
    #[serde(default)]
    pub lora_ref: Option<String>,
    #[serde(default)]
    pub embedding_ref: Option<String>,
}

impl StyleAssets {
    /// Return a new value with the patch applied; never mutates in place.
    pub fn apply(&self, patch: &StylePatch) -> Self {
        let mut next = self.clone();
        if let Some(v) = &patch.style_refs {
            next.style_refs = v.clone();
        }
        if let Some(v) = &patch.palette {
            next.palette = v.clone();
        }
        if let Some(v) = &patch.descriptor {
            next.descriptor = Some(v.clone());
        }
        if let Some(v) = &patch.medium {
            next.medium = Some(v.clone());
        }
        if let Some(v) = &patch.negative_prompt {
            next.negative_prompt = Some(v.clone());
        }
        if let Some(v) = &patch.lora_ref {
            next.lora_ref = Some(v.clone());
        }
        if let Some(v) = &patch.embedding_ref {
            next.embedding_ref = Some(v.clone());
        }
        next
    }
}

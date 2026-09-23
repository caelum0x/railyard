//! Concrete per-modality adapters. Each maps a locked version's typed assets
//! onto the provider-neutral [`ConditioningPayload`] for one modality, across all
//! three passport kinds.

mod image;
mod text;
mod video;
mod voice;

pub use image::ImageAdapter;
pub use text::TextAdapter;
pub use video::VideoAdapter;
pub use voice::VoiceAdapter;

use passport_core::{StyleAssets, VoiceAssets};

use crate::adapter::join;
use crate::payload::ConditioningPayload;

/// Shared style mapping for image + video.
pub(crate) fn apply_style(p: &mut ConditioningPayload, s: &StyleAssets) {
    p.reference_images = s.style_refs.clone();
    p.lora_ref = s.lora_ref.clone();
    p.embedding_ref = s.embedding_ref.clone();
    p.style_prompt = join([s.descriptor.as_deref(), s.medium.as_deref()]);
    p.negative_prompt = s.negative_prompt.clone().filter(|n| !n.trim().is_empty());
    if !s.palette.is_empty() {
        p.extra.insert("palette".into(), s.palette.join(","));
    }
}

/// Shared voice mapping for voice + video. The per-provider `voice_id` is
/// filled from a registered trained artifact by the registry.
pub(crate) fn apply_voice(p: &mut ConditioningPayload, v: &VoiceAssets) {
    p.reference_audio = v
        .sample_refs
        .iter()
        .filter(|r| !r.trim().is_empty())
        .cloned()
        .collect();
    if let Some(lang) = &v.language {
        p.extra.insert("language".into(), lang.clone());
    }
    if let Some(accent) = &v.accent {
        p.extra.insert("accent".into(), accent.clone());
    }
    if let Some(delivery) = &v.delivery {
        p.text_directives.push(format!("delivery: {delivery}"));
    }
}

//! `resolve(passport_id, provider, modality)` — turn a *locked* passport version
//! into a provider-specific conditioning payload.
//!
//! The mapping is primarily driven by `modality` (image / voice / video / text)
//! and the passport `kind`. `provider` is carried through so a downstream Relay
//! adapter can refine the payload per vendor; provider-specific overrides are a
//! documented extension point (see `provider_hint`). The three real, tested
//! mappings are IMAGE, VOICE, and VIDEO, with TEXT provided as a bonus.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{PassportAssets, PassportVersion};
use crate::error::PassportError;

/// The kind of output a caller wants to condition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    #[default]
    Image,
    Voice,
    Video,
    Text,
}

impl Modality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Voice => "voice",
            Self::Video => "video",
            Self::Text => "text",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "image" => Ok(Self::Image),
            "voice" => Ok(Self::Voice),
            "video" => Ok(Self::Video),
            "text" => Ok(Self::Text),
            other => Err(PassportError::ResolveUnavailable {
                provider: "*".into(),
                modality: other.into(),
            }),
        }
    }
}

/// A provider-agnostic conditioning payload. A Relay adapter maps these fields
/// onto a concrete vendor request (Veo, Runway, Kling, HeyGen, ElevenLabs, a
/// local LoRA, …). Only the fields relevant to the requested modality are set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditioningPayload {
    pub passport_id: String,
    pub version: i64,
    pub provider: String,
    pub modality: Modality,
    /// Reference images (faces, logo, watermark) to condition on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_images: Vec<String>,
    /// LoRA / embedding reference for consistent visual identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_ref: Option<String>,
    /// Talking-head / avatar reference for video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_ref: Option<String>,
    /// Provider voice identity for voice/video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// Assembled style prompt fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_prompt: Option<String>,
    /// Free-form text directives (tone, hooks, brand voice).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_directives: Vec<String>,
    /// Structured extras a vendor adapter may consume (palette, platform, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl ConditioningPayload {
    fn base(passport_id: &str, version: i64, provider: &str, modality: Modality) -> Self {
        Self {
            passport_id: passport_id.to_string(),
            version,
            provider: provider.to_string(),
            modality,
            ..Default::default()
        }
    }

    /// True when nothing usable was resolved for the requested modality.
    fn is_empty(&self) -> bool {
        self.reference_images.is_empty()
            && self.lora_ref.is_none()
            && self.avatar_ref.is_none()
            && self.voice_id.is_none()
            && self.style_prompt.is_none()
            && self.text_directives.is_empty()
            && self.extra.is_empty()
    }
}

/// Resolve a locked version into a conditioning payload for `provider` + `modality`.
///
/// Fails closed: an unlocked version never resolves (callers must lock first),
/// and a modality with no usable conditioning returns `ResolveUnavailable`
/// rather than an empty payload.
pub fn resolve(
    version: &PassportVersion,
    provider: &str,
    modality: Modality,
) -> Result<ConditioningPayload, PassportError> {
    if !version.is_locked() {
        return Err(PassportError::VersionNotLocked);
    }

    let mut payload =
        ConditioningPayload::base(&version.passport_id, version.version, provider, modality);

    match (&version.assets, modality) {
        // ---- CHARACTER ----------------------------------------------------
        (PassportAssets::Character(c), Modality::Image) => {
            payload.reference_images = c.face_refs.clone();
            payload.lora_ref = c.lora_ref.clone();
            payload.style_prompt = join_prompt([c.style.as_deref(), c.wardrobe.as_deref()]);
        }
        (PassportAssets::Character(c), Modality::Voice) => {
            payload.voice_id = c.voice_id.clone();
            if let Some(p) = &c.personality {
                payload.text_directives.push(format!("tone: {p}"));
            }
        }
        (PassportAssets::Character(c), Modality::Video) => {
            payload.reference_images = c.face_refs.clone();
            payload.avatar_ref = c.avatar_ref.clone();
            payload.lora_ref = c.lora_ref.clone();
            payload.voice_id = c.voice_id.clone();
            payload.style_prompt = join_prompt([c.style.as_deref(), c.wardrobe.as_deref()]);
        }
        (PassportAssets::Character(c), Modality::Text) => {
            if let Some(p) = &c.personality {
                payload.text_directives.push(format!("persona: {p}"));
            }
            if let Some(s) = &c.style {
                payload.text_directives.push(format!("style: {s}"));
            }
        }

        // ---- BRAND --------------------------------------------------------
        (PassportAssets::Brand(b), Modality::Image | Modality::Video) => {
            payload.reference_images = [b.logo_ref.clone(), b.watermark_ref.clone()]
                .into_iter()
                .flatten()
                .collect();
            payload.style_prompt = join_prompt([b.typography.as_deref(), b.voice_tone.as_deref()]);
            if !b.palette.is_empty() {
                payload
                    .extra
                    .insert("palette".into(), b.palette.join(","));
            }
        }
        (PassportAssets::Brand(b), Modality::Voice | Modality::Text) => {
            if let Some(v) = &b.voice_tone {
                payload.text_directives.push(format!("brand voice: {v}"));
            }
        }

        // ---- PERSONA ------------------------------------------------------
        (PassportAssets::Persona(p), Modality::Image | Modality::Video) => {
            if let Some(platform) = &p.platform {
                payload.extra.insert("platform".into(), platform.clone());
            }
            payload.style_prompt = join_prompt([p.audience.as_deref()]);
        }
        (PassportAssets::Persona(p), Modality::Voice | Modality::Text) => {
            if let Some(lang) = &p.language {
                payload.extra.insert("language".into(), lang.clone());
            }
            if let Some(audience) = &p.audience {
                payload
                    .text_directives
                    .push(format!("audience: {audience}"));
            }
            for hook in &p.hooks {
                payload.text_directives.push(format!("hook: {hook}"));
            }
        }
    }

    if payload.is_empty() {
        return Err(PassportError::ResolveUnavailable {
            provider: provider.to_string(),
            modality: modality.as_str().to_string(),
        });
    }
    Ok(payload)
}

fn join_prompt<const N: usize>(parts: [Option<&str>; N]) -> Option<String> {
    let joined = parts
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CharacterAssets, VersionStatus};
    use chrono::Utc;

    fn locked_character() -> PassportVersion {
        let now = Utc::now();
        PassportVersion {
            passport_id: "psp_1".into(),
            version: 1,
            status: VersionStatus::Locked,
            assets: PassportAssets::Character(CharacterAssets {
                face_refs: vec!["face_a".into(), "face_b".into()],
                voice_id: Some("eleven-42".into()),
                avatar_ref: Some("avatar_x".into()),
                lora_ref: Some("lora_y".into()),
                style: Some("cinematic".into()),
                personality: Some("warm, witty".into()),
                wardrobe: Some("denim jacket".into()),
            }),
            created_at: now,
            updated_at: now,
            locked_at: Some(now),
        }
    }

    #[test]
    fn image_maps_faces_and_lora_and_style() {
        let p = resolve(&locked_character(), "runway", Modality::Image).unwrap();
        assert_eq!(p.reference_images, vec!["face_a", "face_b"]);
        assert_eq!(p.lora_ref.as_deref(), Some("lora_y"));
        assert_eq!(p.style_prompt.as_deref(), Some("cinematic, denim jacket"));
        assert!(p.voice_id.is_none());
    }

    #[test]
    fn voice_maps_voice_id() {
        let p = resolve(&locked_character(), "elevenlabs", Modality::Voice).unwrap();
        assert_eq!(p.voice_id.as_deref(), Some("eleven-42"));
        assert!(p.reference_images.is_empty());
    }

    #[test]
    fn video_maps_avatar_faces_voice() {
        let p = resolve(&locked_character(), "heygen", Modality::Video).unwrap();
        assert_eq!(p.avatar_ref.as_deref(), Some("avatar_x"));
        assert_eq!(p.voice_id.as_deref(), Some("eleven-42"));
        assert_eq!(p.reference_images.len(), 2);
    }

    #[test]
    fn draft_version_never_resolves() {
        let mut v = locked_character();
        v.status = VersionStatus::Draft;
        assert_eq!(
            resolve(&v, "runway", Modality::Image),
            Err(PassportError::VersionNotLocked)
        );
    }
}

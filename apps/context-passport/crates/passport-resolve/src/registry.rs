//! The provider registry: pick an adapter per modality (with optional
//! named-provider overrides), build the payload, and apply capability-driven
//! fail-closed rules.

use std::collections::HashMap;

use passport_core::{PassportError, PassportVersion};

use crate::adapter::ProviderAdapter;
use crate::adapters::{ImageAdapter, TextAdapter, VideoAdapter, VoiceAdapter};
use crate::capability::{capability, IdentityMechanism};
use crate::modality::Modality;
use crate::payload::ConditioningPayload;

/// Holds the default per-modality adapters plus any named `(provider, modality)`
/// overrides. The default registry is enough for the Sx0 mappings; overrides are
/// the vendor-refinement extension point.
pub struct ProviderRegistry {
    image: Box<dyn ProviderAdapter>,
    voice: Box<dyn ProviderAdapter>,
    video: Box<dyn ProviderAdapter>,
    text: Box<dyn ProviderAdapter>,
    overrides: HashMap<(String, Modality), Box<dyn ProviderAdapter>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self {
            image: Box::new(ImageAdapter),
            voice: Box::new(VoiceAdapter),
            video: Box::new(VideoAdapter),
            text: Box::new(TextAdapter),
            overrides: HashMap::new(),
        }
    }
}

impl ProviderRegistry {
    /// Register a vendor-specific adapter for one `(provider, modality)`.
    pub fn with_override(
        mut self,
        provider: impl Into<String>,
        modality: Modality,
        adapter: Box<dyn ProviderAdapter>,
    ) -> Self {
        self.overrides
            .insert((provider.into().to_ascii_lowercase(), modality), adapter);
        self
    }

    fn adapter(&self, provider: &str, modality: Modality) -> &dyn ProviderAdapter {
        if let Some(a) = self
            .overrides
            .get(&(provider.to_ascii_lowercase(), modality))
        {
            return a.as_ref();
        }
        match modality {
            Modality::Image => self.image.as_ref(),
            Modality::Voice => self.voice.as_ref(),
            Modality::Video => self.video.as_ref(),
            Modality::Text => self.text.as_ref(),
        }
    }

    /// Resolve a locked version into conditioning. Fail-closed:
    /// - an unlocked version never resolves;
    /// - a provider with no identity path for the modality is `resolve_unavailable`;
    /// - a Family-B provider missing its minted artifact is `needs_artifact`;
    /// - an otherwise empty payload is `resolve_unavailable`.
    pub fn resolve(
        &self,
        version: &PassportVersion,
        provider: &str,
        modality: Modality,
    ) -> Result<ConditioningPayload, PassportError> {
        if !version.is_locked() {
            return Err(PassportError::VersionNotLocked);
        }

        let cap = capability(provider, modality);
        if cap.mechanism == IdentityMechanism::Unsupported {
            return Err(PassportError::ResolveUnavailable {
                provider: provider.to_string(),
                modality: modality.as_str().to_string(),
            });
        }

        let payload = self
            .adapter(provider, modality)
            .build(version, provider, modality);

        // Family-B providers need their minted artifact present in the payload.
        if cap.requires_artifact() {
            let has_artifact = match cap.mechanism {
                IdentityMechanism::VoiceClone => payload.voice_id.is_some(),
                IdentityMechanism::AvatarEnrollment => payload.avatar_ref.is_some(),
                IdentityMechanism::TrainedImageArtifact => payload.lora_ref.is_some(),
                _ => true,
            };
            if !has_artifact {
                return Err(PassportError::NeedsArtifact {
                    provider: provider.to_string(),
                    modality: modality.as_str().to_string(),
                });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use passport_core::{CharacterAssets, PassportAssets, Rights, VersionStatus};

    fn locked_character(assets: CharacterAssets) -> PassportVersion {
        let now = Utc::now();
        PassportVersion {
            passport_id: "psp_1".into(),
            version: 1,
            status: VersionStatus::Locked,
            assets: PassportAssets::Character(assets),
            rights: Rights::default(),
            content_digest: None,
            created_at: now,
            updated_at: now,
            locked_at: Some(now),
        }
    }

    fn full() -> CharacterAssets {
        CharacterAssets {
            face_refs: vec!["face_a".into(), "face_b".into()],
            voice_id: Some("eleven-42".into()),
            avatar_ref: Some("avatar_x".into()),
            lora_ref: Some("lora_y".into()),
            style: Some("cinematic".into()),
            personality: Some("warm".into()),
            wardrobe: Some("denim".into()),
        }
    }

    #[test]
    fn image_maps_faces_and_lora() {
        let r = ProviderRegistry::default();
        let p = r
            .resolve(&locked_character(full()), "runway", Modality::Image)
            .unwrap();
        assert_eq!(p.reference_images, vec!["face_a", "face_b"]);
        assert_eq!(p.lora_ref.as_deref(), Some("lora_y"));
    }

    #[test]
    fn voice_maps_voice_id() {
        let r = ProviderRegistry::default();
        let p = r
            .resolve(&locked_character(full()), "elevenlabs", Modality::Voice)
            .unwrap();
        assert_eq!(p.voice_id.as_deref(), Some("eleven-42"));
    }

    #[test]
    fn video_maps_avatar_and_voice() {
        let r = ProviderRegistry::default();
        let p = r
            .resolve(&locked_character(full()), "heygen", Modality::Video)
            .unwrap();
        assert_eq!(p.avatar_ref.as_deref(), Some("avatar_x"));
        assert_eq!(p.voice_id.as_deref(), Some("eleven-42"));
    }

    #[test]
    fn missing_voice_clone_is_needs_artifact() {
        let mut assets = full();
        assets.voice_id = None;
        let r = ProviderRegistry::default();
        assert_eq!(
            r.resolve(&locked_character(assets), "elevenlabs", Modality::Voice),
            Err(PassportError::NeedsArtifact {
                provider: "elevenlabs".into(),
                modality: "voice".into()
            })
        );
    }

    #[test]
    fn missing_heygen_avatar_is_needs_artifact() {
        let mut assets = full();
        assets.avatar_ref = None;
        let r = ProviderRegistry::default();
        assert!(matches!(
            r.resolve(&locked_character(assets), "heygen", Modality::Video),
            Err(PassportError::NeedsArtifact { .. })
        ));
    }

    #[test]
    fn draft_never_resolves() {
        let mut v = locked_character(full());
        v.status = VersionStatus::Draft;
        assert_eq!(
            ProviderRegistry::default().resolve(&v, "runway", Modality::Image),
            Err(PassportError::VersionNotLocked)
        );
    }

    #[test]
    fn stability_image_has_no_identity_path() {
        let r = ProviderRegistry::default();
        assert!(matches!(
            r.resolve(&locked_character(full()), "stability", Modality::Image),
            Err(PassportError::ResolveUnavailable { .. })
        ));
    }
}

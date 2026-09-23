//! The provider registry: pick an adapter per modality (with optional
//! named-provider overrides), build the payload, and apply capability-driven
//! fail-closed rules.

use std::collections::HashMap;

use passport_core::{ArtifactKind, PassportAssets, PassportError, PassportVersion, TrainedArtifact};

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
        self.resolve_with_artifacts(version, provider, modality, &[])
    }

    /// Resolve like [`Self::resolve`], additionally conditioning on the ready
    /// [`TrainedArtifact`]s registered for this version and provider. A
    /// registered artifact is provider-specific, so it takes precedence over a
    /// generic id carried inline in the assets. Only when neither exists does a
    /// Family-B provider fail with `needs_artifact`.
    pub fn resolve_with_artifacts(
        &self,
        version: &PassportVersion,
        provider: &str,
        modality: Modality,
        artifacts: &[TrainedArtifact],
    ) -> Result<ConditioningPayload, PassportError> {
        if !version.is_locked() {
            return Err(PassportError::VersionNotLocked);
        }

        let cap = capability(provider, modality);
        let kind_has_no_path = matches!(
            (&version.assets, modality),
            (PassportAssets::Voice(_), Modality::Image) | (PassportAssets::Style(_), Modality::Voice)
        );
        if cap.mechanism == IdentityMechanism::Unsupported || kind_has_no_path {
            return Err(PassportError::ResolveUnavailable {
                provider: provider.to_string(),
                modality: modality.as_str().to_string(),
            });
        }

        let mut payload = self
            .adapter(provider, modality)
            .build(version, provider, modality);
        apply_artifacts(&mut payload, version.version, provider, modality, artifacts);

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

/// Overlay the ready artifacts for `(version, provider)` onto the payload.
fn apply_artifacts(
    payload: &mut ConditioningPayload,
    version: i64,
    provider: &str,
    modality: Modality,
    artifacts: &[TrainedArtifact],
) {
    for artifact in artifacts.iter().filter(|a| a.is_ready_for(provider, version)) {
        let Some(reference) = artifact.artifact_ref.clone() else {
            continue;
        };
        let applied = match (artifact.kind, modality) {
            (ArtifactKind::VoiceClone, Modality::Voice | Modality::Video) => {
                payload.voice_id = Some(reference);
                true
            }
            (ArtifactKind::Avatar, Modality::Video) => {
                payload.avatar_ref = Some(reference);
                true
            }
            (ArtifactKind::Lora, Modality::Image | Modality::Video) => {
                payload.lora_ref = Some(reference);
                true
            }
            _ => false,
        };
        if applied {
            payload.extra.insert(
                format!("artifact.{}", artifact.kind.as_str()),
                artifact.id.clone(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use passport_core::{
        BrandAssets, CharacterAssets, PassportAssets, PersonaAssets, Rights, VersionStatus,
    };

    fn locked_version(assets: PassportAssets) -> PassportVersion {
        let now = Utc::now();
        PassportVersion {
            passport_id: "psp_1".into(),
            version: 1,
            status: VersionStatus::Locked,
            assets,
            rights: Rights::default(),
            content_digest: None,
            created_at: now,
            updated_at: now,
            locked_at: Some(now),
        }
    }

    fn locked_character(assets: CharacterAssets) -> PassportVersion {
        locked_version(PassportAssets::Character(assets))
    }

    fn full() -> CharacterAssets {
        CharacterAssets {
            face_refs: vec!["face_a".into(), "face_b".into()],
            voice_id: Some("eleven-42".into()),
            avatar_ref: Some("avatar_x".into()),
            lora_ref: Some("lora_y".into()),
            embedding_ref: Some("embed_z".into()),
            style: Some("cinematic".into()),
            personality: Some("warm".into()),
            wardrobe: Some("denim".into()),
        }
    }

    #[test]
    fn image_maps_faces_lora_and_embedding() {
        let r = ProviderRegistry::default();
        let p = r
            .resolve(&locked_character(full()), "runway", Modality::Image)
            .unwrap();
        assert_eq!(p.reference_images, vec!["face_a", "face_b"]);
        assert_eq!(p.lora_ref.as_deref(), Some("lora_y"));
        assert_eq!(p.embedding_ref.as_deref(), Some("embed_z"));
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

    #[test]
    fn missing_hedra_avatar_is_needs_artifact() {
        // Hedra is Family B like HeyGen: absent avatar must fail closed, not
        // silently fall back to face references.
        let mut assets = full();
        assets.avatar_ref = None;
        let r = ProviderRegistry::default();
        assert!(matches!(
            r.resolve(&locked_character(assets), "hedra", Modality::Video),
            Err(PassportError::NeedsArtifact { .. })
        ));
    }

    #[test]
    fn brand_image_maps_logo_watermark_and_palette() {
        let assets = PassportAssets::Brand(BrandAssets {
            logo_ref: Some("logo_1".into()),
            watermark_ref: Some("wm_1".into()),
            palette: vec!["#000".into(), "#fff".into()],
            typography: Some("grotesk".into()),
            voice_tone: Some("bold".into()),
        });
        let p = ProviderRegistry::default()
            .resolve(&locked_version(assets), "runway", Modality::Image)
            .unwrap();
        assert_eq!(p.reference_images, vec!["logo_1", "wm_1"]);
        assert_eq!(p.extra.get("palette").map(String::as_str), Some("#000,#fff"));
        assert_eq!(p.style_prompt.as_deref(), Some("grotesk, bold"));
    }

    #[test]
    fn persona_text_maps_audience_language_and_hooks() {
        let assets = PassportAssets::Persona(PersonaAssets {
            audience: Some("gen-z founders".into()),
            language: Some("en".into()),
            platform: Some("tiktok".into()),
            hooks: vec!["stop scrolling".into(), "here is why".into()],
        });
        let p = ProviderRegistry::default()
            .resolve(&locked_version(assets), "gpt-4", Modality::Text)
            .unwrap();
        assert_eq!(p.extra.get("language").map(String::as_str), Some("en"));
        assert!(p
            .text_directives
            .contains(&"audience: gen-z founders".to_string()));
        assert!(p
            .text_directives
            .contains(&"hook: stop scrolling".to_string()));
    }

    #[test]
    fn persona_video_maps_platform_extra() {
        let assets = PassportAssets::Persona(PersonaAssets {
            platform: Some("reels".into()),
            ..Default::default()
        });
        let p = ProviderRegistry::default()
            .resolve(&locked_version(assets), "runway", Modality::Video)
            .unwrap();
        assert_eq!(p.extra.get("platform").map(String::as_str), Some("reels"));
    }

    #[test]
    fn empty_payload_is_resolve_unavailable() {
        // A locked-but-blank persona yields nothing usable for image → the
        // fail-closed empty-payload branch (distinct from capability-unsupported).
        let assets = PassportAssets::Persona(PersonaAssets::default());
        assert!(matches!(
            ProviderRegistry::default().resolve(
                &locked_version(assets),
                "runway",
                Modality::Image
            ),
            Err(PassportError::ResolveUnavailable { .. })
        ));
    }

    #[test]
    fn override_adapter_is_used_for_its_provider_and_modality() {
        struct StampAdapter;
        impl ProviderAdapter for StampAdapter {
            fn build(
                &self,
                version: &PassportVersion,
                provider: &str,
                modality: Modality,
            ) -> ConditioningPayload {
                let mut p = ConditioningPayload::base(
                    &version.passport_id,
                    version.version,
                    provider,
                    modality,
                );
                p.text_directives.push("override".into());
                p
            }
        }
        let r = ProviderRegistry::default().with_override(
            "runway",
            Modality::Image,
            Box::new(StampAdapter),
        );
        let p = r
            .resolve(&locked_character(full()), "RunWay", Modality::Image)
            .unwrap();
        assert_eq!(p.text_directives, vec!["override".to_string()]);
        // A different modality still uses the default adapter.
        let d = r
            .resolve(&locked_character(full()), "runway", Modality::Video)
            .unwrap();
        assert!(d.text_directives.is_empty());
    }

    fn artifact(kind: passport_core::ArtifactKind, provider: &str, reference: &str) -> TrainedArtifact {
        let now = Utc::now();
        TrainedArtifact {
            id: "art_1".into(),
            passport_id: "psp_1".into(),
            version: 1,
            source_digest: None,
            provider: provider.into(),
            kind,
            artifact_ref: Some(reference.into()),
            provider_model: None,
            status: passport_core::ArtifactStatus::Ready,
            failure_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn registered_voice_clone_satisfies_needs_artifact_for_its_provider_only() {
        let assets = PassportAssets::Voice(passport_core::VoiceAssets {
            sample_refs: vec!["r2://voice/sample-1.wav".into()],
            language: Some("en-US".into()),
            ..Default::default()
        });
        let v = locked_version(assets);
        let r = ProviderRegistry::default();
        assert!(matches!(
            r.resolve(&v, "elevenlabs", Modality::Voice),
            Err(PassportError::NeedsArtifact { .. })
        ));
        let arts = [artifact(passport_core::ArtifactKind::VoiceClone, "elevenlabs", "el_voice_9")];
        let p = r
            .resolve_with_artifacts(&v, "ElevenLabs", Modality::Voice, &arts)
            .unwrap();
        assert_eq!(p.voice_id.as_deref(), Some("el_voice_9"));
        assert_eq!(p.reference_audio, vec!["r2://voice/sample-1.wav"]);
        // Another vendor's clone is not portable.
        assert!(matches!(
            r.resolve_with_artifacts(&v, "cartesia", Modality::Voice, &arts),
            Err(PassportError::NeedsArtifact { .. })
        ));
    }

    #[test]
    fn style_image_maps_refs_prompt_and_negative() {
        let assets = PassportAssets::Style(passport_core::StyleAssets {
            style_refs: vec!["ref_a".into()],
            descriptor: Some("moody neon".into()),
            medium: Some("35mm film".into()),
            negative_prompt: Some("blurry".into()),
            palette: vec!["#111".into()],
            ..Default::default()
        });
        let p = ProviderRegistry::default()
            .resolve(&locked_version(assets.clone()), "runway", Modality::Image)
            .unwrap();
        assert_eq!(p.reference_images, vec!["ref_a"]);
        assert_eq!(p.style_prompt.as_deref(), Some("moody neon, 35mm film"));
        assert_eq!(p.negative_prompt.as_deref(), Some("blurry"));
        assert!(matches!(
            ProviderRegistry::default().resolve(&locked_version(assets), "elevenlabs", Modality::Voice),
            Err(PassportError::ResolveUnavailable { .. })
        ));
    }
}

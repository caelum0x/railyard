//! Provider capability model.
//!
//! From the provider research, identity conditioning splits into two families:
//! **A — inference-time reference** (portable: reference images, persona text)
//! and **B — trained/enrolled per-provider artifact** (non-portable: LoRA,
//! HeyGen avatar, every voice clone). The resolver routes a neutral passport into
//! whichever family a target provider supports, and fails closed with
//! `needs_artifact` when a Family-B provider lacks its minted artifact.

use crate::modality::Modality;

/// The mechanism a provider uses to lock identity for a modality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityMechanism {
    /// Family A: send reference images / first frame each call (portable).
    Reference,
    /// Family B: a provider-minted voice clone token (`voice_id`).
    VoiceClone,
    /// Family B: an enrolled talking-head avatar (`avatar_id`).
    AvatarEnrollment,
    /// Family B: a trained image artifact (LoRA / DreamBooth / custom model).
    TrainedImageArtifact,
    /// The provider has no identity path for this modality.
    Unsupported,
}

/// What a given `(provider, modality)` can accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderCapability {
    pub mechanism: IdentityMechanism,
}

impl ProviderCapability {
    /// True when identity requires a Family-B minted artifact.
    pub const fn requires_artifact(self) -> bool {
        matches!(
            self.mechanism,
            IdentityMechanism::VoiceClone
                | IdentityMechanism::AvatarEnrollment
                | IdentityMechanism::TrainedImageArtifact
        )
    }
}

/// Resolve a provider+modality to its capability. Unknown providers get the
/// portable default for that modality (voice is always Family B — no voice id is
/// portable across vendors).
pub fn capability(provider: &str, modality: Modality) -> ProviderCapability {
    let p = provider.to_ascii_lowercase();
    let mechanism = match modality {
        // Voice identity is never portable: it is always a per-provider clone.
        Modality::Voice => IdentityMechanism::VoiceClone,
        // Text/persona is fully portable plain text.
        Modality::Text => IdentityMechanism::Reference,
        Modality::Image => {
            if p.contains("lora") || p.contains("dreambooth") || p.contains("firefly") {
                IdentityMechanism::TrainedImageArtifact
            } else if p == "stability" || p.contains("stable-diffusion-core") {
                // Stability's API has no face/identity endpoint.
                IdentityMechanism::Unsupported
            } else {
                IdentityMechanism::Reference
            }
        }
        Modality::Video => {
            if p.contains("heygen") {
                IdentityMechanism::AvatarEnrollment
            } else {
                IdentityMechanism::Reference
            }
        }
    };
    ProviderCapability { mechanism }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_always_needs_an_artifact() {
        assert!(capability("elevenlabs", Modality::Voice).requires_artifact());
        assert!(capability("anything", Modality::Voice).requires_artifact());
    }

    #[test]
    fn generic_image_and_video_are_portable_reference() {
        assert!(!capability("runway", Modality::Video).requires_artifact());
        assert!(!capability("gpt-image", Modality::Image).requires_artifact());
    }

    #[test]
    fn heygen_video_and_lora_image_need_artifacts() {
        assert!(capability("heygen", Modality::Video).requires_artifact());
        assert!(capability("replicate-lora", Modality::Image).requires_artifact());
    }
}

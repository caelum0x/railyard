//! Resolve a *locked* passport version into a provider-specific conditioning
//! payload — the key infrastructure method from `VISION.md`.
//!
//! Mapping is driven by `modality` (image/voice/video/text) and passport `kind`,
//! through a [`ProviderRegistry`] of [`ProviderAdapter`]s, routed by a provider
//! [`capability`] model. Identity conditioning splits into two families:
//! portable inference-time references (Family A) and non-portable per-provider
//! trained artifacts (Family B — voice clones, HeyGen avatars, LoRAs).
//! Resolution is **fail-closed**: an unlocked version never resolves; a Family-B
//! provider missing its artifact returns `needs_artifact`; a modality with no
//! usable conditioning returns `resolve_unavailable`.

pub mod adapter;
pub mod adapters;
pub mod capability;
pub mod modality;
pub mod payload;
pub mod registry;

pub use adapter::ProviderAdapter;
pub use capability::{capability, IdentityMechanism, ProviderCapability};
pub use modality::Modality;
pub use payload::ConditioningPayload;
pub use registry::ProviderRegistry;

use passport_core::{PassportError, PassportVersion};

/// Resolve with the default registry (generic per-modality adapters).
pub fn resolve(
    version: &PassportVersion,
    provider: &str,
    modality: Modality,
) -> Result<ConditioningPayload, PassportError> {
    ProviderRegistry::default().resolve(version, provider, modality)
}

//! The adapter seam: turn a locked version's assets into a conditioning payload
//! for one modality. Named providers can supply refined adapters via the registry.

use passport_core::PassportVersion;

use crate::modality::Modality;
use crate::payload::ConditioningPayload;

pub trait ProviderAdapter: Send + Sync {
    /// Build the conditioning payload for this version + provider + modality.
    /// The caller guarantees the version is locked.
    fn build(
        &self,
        version: &PassportVersion,
        provider: &str,
        modality: Modality,
    ) -> ConditioningPayload;
}

/// Join non-empty fragments into a single prompt string.
pub(crate) fn join<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let joined = parts
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

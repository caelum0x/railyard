//! Trained / enrolled per-provider artifacts derived from a locked version.
//!
//! Family-B providers cannot condition on portable references: every voice
//! vendor needs its own clone, HeyGen/Hedra need an enrolled avatar, and a
//! LoRA host needs a trained model. A [`TrainedArtifact`] records that minted
//! artifact for ONE `(passport, locked version, provider, kind)`. It is derived
//! data, not identity: registering one never changes the locked version or its
//! content digest, so artifacts can be added to (and revoked from) a locked
//! version at any time. `resolve` looks up the ready artifact for the target
//! provider before failing with `needs_artifact`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::VersionDigest;
use crate::error::PassportError;

/// Which Family-B mechanism an artifact satisfies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A provider voice clone (`voice_id`) — voice and talking-head video.
    VoiceClone,
    /// An enrolled talking-head avatar (HeyGen / Hedra `avatar_id`).
    Avatar,
    /// A trained image model (LoRA / DreamBooth / custom model id).
    Lora,
}

impl ArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VoiceClone => "voice_clone",
            Self::Avatar => "avatar",
            Self::Lora => "lora",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "voice_clone" => Ok(Self::VoiceClone),
            "avatar" => Ok(Self::Avatar),
            "lora" => Ok(Self::Lora),
            other => Err(PassportError::Invalid(format!(
                "unknown artifact kind '{other}' (expected voice_clone, avatar or lora)"
            ))),
        }
    }
}

/// Lifecycle of a provider artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    /// Training / enrollment submitted to the provider, not usable yet.
    Pending,
    /// Usable: `resolve` will condition on it.
    Ready,
    /// The provider job failed; see `failure_reason`.
    Failed,
    /// Withdrawn (consent revoked, superseded, or deleted at the provider).
    Revoked,
}

impl ArtifactStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            "revoked" => Ok(Self::Revoked),
            other => Err(PassportError::Invalid(format!(
                "unknown artifact status '{other}'"
            ))),
        }
    }

    /// Pending and ready artifacts are "active": at most one per
    /// `(passport, version, provider, kind)`.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Ready)
    }
}

/// A provider-minted artifact bound to one locked passport version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainedArtifact {
    pub id: String,
    pub passport_id: String,
    /// The LOCKED version this artifact was trained / enrolled from.
    pub version: i64,
    /// Content digest of that version at registration time (tamper evidence:
    /// the artifact provably came from exactly this identity snapshot).
    #[serde(default)]
    pub source_digest: Option<VersionDigest>,
    /// Provider id, lower-cased (`elevenlabs`, `heygen`, `replicate-lora`, ...).
    pub provider: String,
    pub kind: ArtifactKind,
    /// The provider's id for the artifact (voice id, avatar id, model id).
    /// Required once ready; may be absent while pending.
    #[serde(default)]
    pub artifact_ref: Option<String>,
    /// Provider model / engine version the artifact was minted with.
    #[serde(default)]
    pub provider_model: Option<String>,
    pub status: ArtifactStatus,
    #[serde(default)]
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TrainedArtifact {
    /// `true` when `resolve` may condition on this artifact for `provider`.
    pub fn is_ready_for(&self, provider: &str, version: i64) -> bool {
        self.status == ArtifactStatus::Ready
            && self.version == version
            && self.provider.eq_ignore_ascii_case(provider.trim())
            && self.artifact_ref.as_deref().is_some_and(|r| !r.is_empty())
    }
}

/// Longest provider id / artifact ref / reason accepted.
pub const MAX_PROVIDER_LEN: usize = 64;
pub const MAX_ARTIFACT_REF_LEN: usize = 1_024;
pub const MAX_FAILURE_REASON_LEN: usize = 1_000;

/// Normalize and validate a provider id: lower-case, 1..=64 chars of
/// `[a-z0-9._-]`.
pub fn normalize_provider(provider: &str) -> Result<String, PassportError> {
    let p = provider.trim().to_ascii_lowercase();
    if p.is_empty()
        || p.len() > MAX_PROVIDER_LEN
        || !p
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(PassportError::Invalid(
            "provider must be 1-64 characters of a-z, 0-9, '-', '_' or '.'".into(),
        ));
    }
    Ok(p)
}

/// Validate an optional artifact ref (bounded, non-blank when present).
pub fn validate_artifact_ref(value: Option<&str>) -> Result<Option<String>, PassportError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.len() > MAX_ARTIFACT_REF_LEN => Err(PassportError::Invalid(format!(
            "artifact_ref must be at most {MAX_ARTIFACT_REF_LEN} characters"
        ))),
        Some(v) => Ok(Some(v.to_string())),
    }
}

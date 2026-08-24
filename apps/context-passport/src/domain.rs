//! Pure Character Lock Passport domain types.
//!
//! A `Passport` is the durable identity aggregate. Its locked asset fields live
//! in immutable `PassportVersion`s. Editing a draft version merges a typed
//! patch; locking freezes it; branching produces a new draft from a locked one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::PassportError;

/// What kind of identity a passport locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassportKind {
    Character,
    Brand,
    Persona,
}

impl PassportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Character => "character",
            Self::Brand => "brand",
            Self::Persona => "persona",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "character" => Ok(Self::Character),
            "brand" => Ok(Self::Brand),
            "persona" => Ok(Self::Persona),
            other => Err(PassportError::Storage(format!(
                "unknown passport kind {other}"
            ))),
        }
    }
}

/// A version is a mutable draft until it is locked, then immutable forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Draft,
    Locked,
}

impl VersionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Locked => "locked",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "draft" => Ok(Self::Draft),
            "locked" => Ok(Self::Locked),
            other => Err(PassportError::Storage(format!(
                "unknown version status {other}"
            ))),
        }
    }

    pub const fn is_locked(self) -> bool {
        matches!(self, Self::Locked)
    }
}

/// The locked, typed asset fields for a CHARACTER identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterAssets {
    /// Face / appearance reference image refs (e.g. R2 keys, content hashes).
    #[serde(default)]
    pub face_refs: Vec<String>,
    /// Provider voice identity (ElevenLabs / etc).
    #[serde(default)]
    pub voice_id: Option<String>,
    /// Rendered avatar reference (HeyGen / talking-head ref).
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

/// The locked, typed asset fields for a BRAND identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandAssets {
    #[serde(default)]
    pub logo_ref: Option<String>,
    /// Palette as hex color strings.
    #[serde(default)]
    pub palette: Vec<String>,
    #[serde(default)]
    pub typography: Option<String>,
    #[serde(default)]
    pub watermark_ref: Option<String>,
    /// Voice-and-tone guidelines.
    #[serde(default)]
    pub voice_tone: Option<String>,
}

/// The locked, typed asset fields for a PERSONA identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaAssets {
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// Target platform conventions (tiktok, reels, shorts, …).
    #[serde(default)]
    pub platform: Option<String>,
    /// Hooks that convert for this persona.
    #[serde(default)]
    pub hooks: Vec<String>,
}

/// Kind-typed asset payload carried by a version. The variant is bound to the
/// passport's `kind`; the store enforces the pairing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PassportAssets {
    Character(CharacterAssets),
    Brand(BrandAssets),
    Persona(PersonaAssets),
}

impl PassportAssets {
    pub const fn kind(&self) -> PassportKind {
        match self {
            Self::Character(_) => PassportKind::Character,
            Self::Brand(_) => PassportKind::Brand,
            Self::Persona(_) => PassportKind::Persona,
        }
    }

    /// An empty asset set for a freshly created draft of the given kind.
    pub fn empty(kind: PassportKind) -> Self {
        match kind {
            PassportKind::Character => Self::Character(CharacterAssets::default()),
            PassportKind::Brand => Self::Brand(BrandAssets::default()),
            PassportKind::Persona => Self::Persona(PersonaAssets::default()),
        }
    }

    /// Merge a typed patch, returning a NEW asset set (never mutates in place).
    /// Absent patch fields keep the current value; present fields overwrite.
    pub fn merged(&self, patch: &AssetsPatch) -> Result<Self, PassportError> {
        match (self, patch) {
            (Self::Character(base), AssetsPatch::Character(p)) => {
                let mut next = base.clone();
                if let Some(v) = &p.face_refs {
                    next.face_refs = v.clone();
                }
                if let Some(v) = &p.voice_id {
                    next.voice_id = Some(v.clone());
                }
                if let Some(v) = &p.avatar_ref {
                    next.avatar_ref = Some(v.clone());
                }
                if let Some(v) = &p.lora_ref {
                    next.lora_ref = Some(v.clone());
                }
                if let Some(v) = &p.style {
                    next.style = Some(v.clone());
                }
                if let Some(v) = &p.personality {
                    next.personality = Some(v.clone());
                }
                if let Some(v) = &p.wardrobe {
                    next.wardrobe = Some(v.clone());
                }
                Ok(Self::Character(next))
            }
            (Self::Brand(base), AssetsPatch::Brand(p)) => {
                let mut next = base.clone();
                if let Some(v) = &p.logo_ref {
                    next.logo_ref = Some(v.clone());
                }
                if let Some(v) = &p.palette {
                    next.palette = v.clone();
                }
                if let Some(v) = &p.typography {
                    next.typography = Some(v.clone());
                }
                if let Some(v) = &p.watermark_ref {
                    next.watermark_ref = Some(v.clone());
                }
                if let Some(v) = &p.voice_tone {
                    next.voice_tone = Some(v.clone());
                }
                Ok(Self::Brand(next))
            }
            (Self::Persona(base), AssetsPatch::Persona(p)) => {
                let mut next = base.clone();
                if let Some(v) = &p.audience {
                    next.audience = Some(v.clone());
                }
                if let Some(v) = &p.language {
                    next.language = Some(v.clone());
                }
                if let Some(v) = &p.platform {
                    next.platform = Some(v.clone());
                }
                if let Some(v) = &p.hooks {
                    next.hooks = v.clone();
                }
                Ok(Self::Persona(next))
            }
            (base, patch) => Err(PassportError::KindMismatch(format!(
                "cannot apply {} patch to {} passport",
                patch.kind().as_str(),
                base.kind().as_str()
            ))),
        }
    }
}

/// A typed, partial edit to a draft version's assets. Vec fields, when present,
/// replace wholesale; scalar `Option` fields set the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetsPatch {
    Character(CharacterPatch),
    Brand(BrandPatch),
    Persona(PersonaPatch),
}

impl AssetsPatch {
    pub const fn kind(&self) -> PassportKind {
        match self {
            Self::Character(_) => PassportKind::Character,
            Self::Brand(_) => PassportKind::Brand,
            Self::Persona(_) => PassportKind::Persona,
        }
    }
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandPatch {
    #[serde(default)]
    pub logo_ref: Option<String>,
    #[serde(default)]
    pub palette: Option<Vec<String>>,
    #[serde(default)]
    pub typography: Option<String>,
    #[serde(default)]
    pub watermark_ref: Option<String>,
    #[serde(default)]
    pub voice_tone: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaPatch {
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub hooks: Option<Vec<String>>,
}

/// One immutable-once-locked snapshot of a passport's asset fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassportVersion {
    pub passport_id: String,
    pub version: i64,
    pub status: VersionStatus,
    pub assets: PassportAssets,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub locked_at: Option<DateTime<Utc>>,
}

impl PassportVersion {
    pub const fn is_locked(&self) -> bool {
        self.status.is_locked()
    }
}

/// The durable identity aggregate. `version` is the head (latest) version number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passport {
    pub id: String,
    pub kind: PassportKind,
    pub owner: String,
    /// Head status: `locked` once the head version is frozen, else `draft`.
    pub status: VersionStatus,
    /// Head version number (monotonic, starts at 1).
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A create request. The passport `id` is generated server-side; `idempotency_key`
/// makes create safe to retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePassport {
    pub idempotency_key: String,
    pub kind: PassportKind,
    pub owner: String,
    /// Optional initial draft assets; omit for an empty draft.
    #[serde(default)]
    pub assets: Option<AssetsPatch>,
}

impl CreatePassport {
    pub fn validate(&self) -> Result<(), PassportError> {
        if self.idempotency_key.trim().is_empty() {
            return Err(PassportError::Invalid(
                "idempotency_key must not be empty".into(),
            ));
        }
        if self.owner.trim().is_empty() {
            return Err(PassportError::Invalid("owner must not be empty".into()));
        }
        if let Some(patch) = &self.assets {
            if patch.kind() != self.kind {
                return Err(PassportError::KindMismatch(format!(
                    "initial assets are {} but passport kind is {}",
                    patch.kind().as_str(),
                    self.kind.as_str()
                )));
            }
        }
        Ok(())
    }
}

/// A `Passport` bundled with a specific version — the shape returned by read APIs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassportView {
    #[serde(flatten)]
    pub passport: Passport,
    pub head: PassportVersion,
}

/// Optional filters for listing passports.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PassportFilter {
    pub owner: Option<String>,
    pub kind: Option<PassportKind>,
    pub limit: Option<i64>,
}

/// Provenance: which passport + version conditioned a generated output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub id: String,
    pub passport_id: String,
    pub version: i64,
    pub provider: String,
    pub modality: String,
    /// Reference to the produced asset (e.g. R2 key, URL, content hash).
    pub output_ref: String,
    pub created_at: DateTime<Utc>,
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Interleave a monotonic process counter so ids are unique even when the
    // clock resolution collides under load.
    format!("{nanos:x}{seq:x}")
}

/// Generate a fresh passport id, e.g. `psp_18f2c...`.
pub fn new_passport_id() -> String {
    format!("psp_{}", unique_suffix())
}

/// Generate a fresh generation-record id, e.g. `gen_18f2c...`.
pub fn new_generation_id() -> String {
    format!("gen_{}", unique_suffix())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overwrites_present_fields_only() {
        let base = PassportAssets::Character(CharacterAssets {
            face_refs: vec!["a".into()],
            style: Some("noir".into()),
            ..Default::default()
        });
        let patch = AssetsPatch::Character(CharacterPatch {
            voice_id: Some("eleven-42".into()),
            ..Default::default()
        });
        let merged = base.merged(&patch).expect("merge");
        let PassportAssets::Character(c) = merged else {
            panic!("kind changed");
        };
        assert_eq!(c.face_refs, vec!["a".to_string()]);
        assert_eq!(c.style.as_deref(), Some("noir"));
        assert_eq!(c.voice_id.as_deref(), Some("eleven-42"));
    }

    #[test]
    fn merge_rejects_kind_mismatch() {
        let base = PassportAssets::empty(PassportKind::Character);
        let patch = AssetsPatch::Brand(BrandPatch::default());
        assert!(matches!(
            base.merged(&patch),
            Err(PassportError::KindMismatch(_))
        ));
    }

    #[test]
    fn generated_ids_are_unique() {
        let a = new_passport_id();
        let b = new_passport_id();
        assert_ne!(a, b);
        assert!(a.starts_with("psp_"));
    }
}

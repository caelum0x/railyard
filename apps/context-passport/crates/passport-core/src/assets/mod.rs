//! Kind-typed asset payloads and their partial-edit patches.
//!
//! The [`PassportAssets`] variant is bound to the passport's `kind`; merging a
//! mismatched patch is a typed error, and merging always returns a NEW value.

mod brand;
mod character;
mod persona;
mod style;
mod voice;

pub use brand::{BrandAssets, BrandPatch};
pub use character::{CharacterAssets, CharacterPatch};
pub use persona::{PersonaAssets, PersonaPatch};
pub use style::{StyleAssets, StylePatch};
pub use voice::{VoiceAssets, VoicePatch};

use serde::{Deserialize, Serialize};

use crate::error::PassportError;
use crate::kind::PassportKind;

/// Kind-typed asset payload carried by a version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PassportAssets {
    Character(CharacterAssets),
    Brand(BrandAssets),
    Persona(PersonaAssets),
    Voice(VoiceAssets),
    Style(StyleAssets),
}

/// A typed, partial edit to a draft version's assets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetsPatch {
    Character(CharacterPatch),
    Brand(BrandPatch),
    Persona(PersonaPatch),
    Voice(VoicePatch),
    Style(StylePatch),
}

impl PassportAssets {
    pub const fn kind(&self) -> PassportKind {
        match self {
            Self::Character(_) => PassportKind::Character,
            Self::Brand(_) => PassportKind::Brand,
            Self::Persona(_) => PassportKind::Persona,
            Self::Voice(_) => PassportKind::Voice,
            Self::Style(_) => PassportKind::Style,
        }
    }

    /// Kind-specific invariants a version must satisfy before it can lock.
    /// Character/brand/persona lock on rights alone; a voice needs recordings
    /// to clone from, and a style needs at least one conditioning channel.
    pub fn validate_for_lock(&self) -> Result<(), PassportError> {
        match self {
            Self::Voice(v) => {
                if v.sample_refs.iter().all(|r| r.trim().is_empty()) {
                    return Err(PassportError::Invalid(
                        "a voice passport needs at least one sample recording ref before it can lock"
                            .into(),
                    ));
                }
                Ok(())
            }
            Self::Style(s) => {
                let has_channel = s.style_refs.iter().any(|r| !r.trim().is_empty())
                    || s.descriptor.as_deref().is_some_and(|d| !d.trim().is_empty())
                    || s.lora_ref.is_some()
                    || s.embedding_ref.is_some();
                if !has_channel {
                    return Err(PassportError::Invalid(
                        "a style passport needs style refs, a descriptor, a LoRA or an embedding before it can lock"
                            .into(),
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// An empty asset set for a freshly created draft of the given kind.
    pub fn empty(kind: PassportKind) -> Self {
        match kind {
            PassportKind::Character => Self::Character(CharacterAssets::default()),
            PassportKind::Brand => Self::Brand(BrandAssets::default()),
            PassportKind::Persona => Self::Persona(PersonaAssets::default()),
            PassportKind::Voice => Self::Voice(VoiceAssets::default()),
            PassportKind::Style => Self::Style(StyleAssets::default()),
        }
    }

    /// Merge a typed patch, returning a NEW asset set. Absent patch fields keep
    /// the current value; present fields overwrite. Kind mismatch is an error.
    pub fn merged(&self, patch: &AssetsPatch) -> Result<Self, PassportError> {
        match (self, patch) {
            (Self::Character(base), AssetsPatch::Character(p)) => {
                Ok(Self::Character(base.apply(p)))
            }
            (Self::Brand(base), AssetsPatch::Brand(p)) => Ok(Self::Brand(base.apply(p))),
            (Self::Persona(base), AssetsPatch::Persona(p)) => Ok(Self::Persona(base.apply(p))),
            (Self::Voice(base), AssetsPatch::Voice(p)) => Ok(Self::Voice(base.apply(p))),
            (Self::Style(base), AssetsPatch::Style(p)) => Ok(Self::Style(base.apply(p))),
            (base, patch) => Err(PassportError::KindMismatch(format!(
                "cannot apply {} patch to {} passport",
                patch.kind().as_str(),
                base.kind().as_str()
            ))),
        }
    }
}

impl AssetsPatch {
    pub const fn kind(&self) -> PassportKind {
        match self {
            Self::Character(_) => PassportKind::Character,
            Self::Brand(_) => PassportKind::Brand,
            Self::Persona(_) => PassportKind::Persona,
            Self::Voice(_) => PassportKind::Voice,
            Self::Style(_) => PassportKind::Style,
        }
    }
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
        let PassportAssets::Character(c) = base.merged(&patch).unwrap() else {
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
}

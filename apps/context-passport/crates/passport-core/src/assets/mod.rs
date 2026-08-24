//! Kind-typed asset payloads and their partial-edit patches.
//!
//! The [`PassportAssets`] variant is bound to the passport's `kind`; merging a
//! mismatched patch is a typed error, and merging always returns a NEW value.

mod brand;
mod character;
mod persona;

pub use brand::{BrandAssets, BrandPatch};
pub use character::{CharacterAssets, CharacterPatch};
pub use persona::{PersonaAssets, PersonaPatch};

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
}

/// A typed, partial edit to a draft version's assets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetsPatch {
    Character(CharacterPatch),
    Brand(BrandPatch),
    Persona(PersonaPatch),
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

    /// Merge a typed patch, returning a NEW asset set. Absent patch fields keep
    /// the current value; present fields overwrite. Kind mismatch is an error.
    pub fn merged(&self, patch: &AssetsPatch) -> Result<Self, PassportError> {
        match (self, patch) {
            (Self::Character(base), AssetsPatch::Character(p)) => {
                Ok(Self::Character(base.apply(p)))
            }
            (Self::Brand(base), AssetsPatch::Brand(p)) => Ok(Self::Brand(base.apply(p))),
            (Self::Persona(base), AssetsPatch::Persona(p)) => Ok(Self::Persona(base.apply(p))),
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

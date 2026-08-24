//! The durable identity aggregate and its create/read shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::assets::AssetsPatch;
use crate::error::PassportError;
use crate::kind::PassportKind;
use crate::rights::Rights;
use crate::version::{PassportVersion, VersionStatus};

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
    /// Optional rights; defaults to a synthetic-original, disclosure-required record.
    #[serde(default)]
    pub rights: Option<Rights>,
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

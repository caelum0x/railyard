//! Immutable-once-locked version snapshots.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::assets::PassportAssets;
use crate::digest::VersionDigest;
use crate::error::PassportError;
use crate::rights::Rights;

/// A version is a mutable draft until locked, then immutable and content-addressed.
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

/// The canonical, hashable body of a version — exactly the fields that define the
/// identity. Timestamps and status live outside it so the content address is
/// stable across storage metadata changes.
#[derive(Debug, Serialize)]
struct VersionBody<'a> {
    passport_id: &'a str,
    version: i64,
    assets: &'a PassportAssets,
    rights: &'a Rights,
}

/// One immutable-once-locked snapshot of a passport's asset fields and rights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassportVersion {
    pub passport_id: String,
    pub version: i64,
    pub status: VersionStatus,
    pub assets: PassportAssets,
    #[serde(default)]
    pub rights: Rights,
    /// Content address, set at lock time; `None` while a draft.
    #[serde(default)]
    pub content_digest: Option<VersionDigest>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub locked_at: Option<DateTime<Utc>>,
}

impl PassportVersion {
    pub const fn is_locked(&self) -> bool {
        self.status.is_locked()
    }

    /// Compute the content digest of the current body (independent of status).
    pub fn compute_digest(&self) -> VersionDigest {
        VersionDigest::of(&VersionBody {
            passport_id: &self.passport_id,
            version: self.version,
            assets: &self.assets,
            rights: &self.rights,
        })
    }

    /// Freeze this draft: validate rights, stamp the content digest and lock time.
    /// Returns a NEW locked version (never mutates the receiver).
    pub fn locked(&self, now: DateTime<Utc>) -> Result<Self, PassportError> {
        self.rights.validate_for_lock()?;
        self.assets.validate_for_lock()?;
        let mut next = self.clone();
        next.status = VersionStatus::Locked;
        next.content_digest = Some(self.compute_digest());
        next.locked_at = Some(now);
        next.updated_at = now;
        Ok(next)
    }
}

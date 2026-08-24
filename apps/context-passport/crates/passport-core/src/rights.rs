//! Likeness rights & synthetic-media disclosure carried by every version.
//!
//! Mirrors the *concepts* in the SAG-AFTRA Digital Replica Rider and the (still
//! pending) NO FAKES Act rather than any single schema, because no machine
//! readable subject-consent standard exists yet. PII is never stored inline:
//! `consent_record_ref` points at the hash of an off-passport signed agreement.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::PassportError;

/// The legal basis for using this identity's likeness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentBasis {
    /// A wholly synthetic, invented identity — no real person depicted.
    SyntheticOriginal,
    /// Derived from a real person under a recorded, time-boxed license.
    RealPersonLicensed,
    /// A composite that blends synthetic and licensed real-person elements.
    Composite,
}

/// Synthetic-media disclosure obligation for assets generated from this identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureLevel {
    None,
    Recommended,
    Required,
}

/// Rights snapshot. Copied (frozen) into each generation's provenance so a later
/// revocation cannot retroactively rewrite whether an asset was licensed when made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rights {
    pub basis: ConsentBasis,
    /// Identifier (or hash) of the depicted person; required for licensed bases.
    #[serde(default)]
    pub likeness_of: Option<String>,
    #[serde(default)]
    pub consent_obtained: bool,
    /// Hash reference to the off-passport signed consent agreement (never inline PII).
    #[serde(default)]
    pub consent_record_ref: Option<String>,
    /// Permitted use categories (advertising, video, stills, …).
    #[serde(default)]
    pub scope: Vec<String>,
    /// Explicitly prohibited uses (political, adult, defamatory, …).
    #[serde(default)]
    pub prohibited_uses: Vec<String>,
    #[serde(default)]
    pub term_start: Option<DateTime<Utc>>,
    /// License end. NO FAKES caps living-person replicas at 10 years.
    #[serde(default)]
    pub term_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub territory: Vec<String>,
    #[serde(default = "default_true")]
    pub revocable: bool,
    pub disclosure: DisclosureLevel,
    /// IPTC digitalSourceType URI stamped onto generated assets.
    #[serde(default = "default_source_type")]
    pub iptc_digital_source_type: String,
}

fn default_true() -> bool {
    true
}

fn default_source_type() -> String {
    "http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia".to_string()
}

impl Default for Rights {
    fn default() -> Self {
        Self {
            basis: ConsentBasis::SyntheticOriginal,
            likeness_of: None,
            consent_obtained: false,
            consent_record_ref: None,
            scope: Vec::new(),
            prohibited_uses: Vec::new(),
            term_start: None,
            term_end: None,
            territory: Vec::new(),
            revocable: true,
            disclosure: DisclosureLevel::Required,
            iptc_digital_source_type: default_source_type(),
        }
    }
}

impl Rights {
    /// A real-person likeness may not be locked without recorded consent.
    /// Enforced at lock time so no immutable version can violate it.
    pub fn validate_for_lock(&self) -> Result<(), PassportError> {
        let licensed = matches!(
            self.basis,
            ConsentBasis::RealPersonLicensed | ConsentBasis::Composite
        );
        if licensed {
            if !self.consent_obtained {
                return Err(PassportError::Invalid(
                    "a real-person or composite likeness requires consent_obtained = true before locking"
                        .into(),
                ));
            }
            if self.likeness_of.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PassportError::Invalid(
                    "a licensed likeness must record likeness_of".into(),
                ));
            }
            if self
                .consent_record_ref
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(PassportError::Invalid(
                    "a licensed likeness must reference a consent_record_ref".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_original_locks_freely() {
        assert!(Rights::default().validate_for_lock().is_ok());
    }

    #[test]
    fn licensed_likeness_requires_consent() {
        let rights = Rights {
            basis: ConsentBasis::RealPersonLicensed,
            ..Default::default()
        };
        assert!(rights.validate_for_lock().is_err());

        let ok = Rights {
            basis: ConsentBasis::RealPersonLicensed,
            consent_obtained: true,
            likeness_of: Some("actor:jane-doe".into()),
            consent_record_ref: Some("ni:///sha-256;abc".into()),
            ..Default::default()
        };
        assert!(ok.validate_for_lock().is_ok());
    }
}

//! Character Lock Passport domain kernel — pure types and invariants, no I/O.
//!
//! A [`Passport`] is the durable identity aggregate; its locked asset fields
//! live in immutable, content-addressed [`PassportVersion`]s. Editing a draft
//! merges a typed [`AssetsPatch`]; locking freezes and digests it; branching
//! forks a new draft from a locked version.
//!
//! Downstream crates (resolve, store, clients, server) depend only on this
//! kernel and on the injectable [`ports`] traits declared here.

pub mod assets;
pub mod digest;
pub mod error;
pub mod ids;
pub mod kind;
pub mod passport;
pub mod ports;
pub mod rights;
pub mod version;

pub use assets::{
    AssetsPatch, BrandAssets, BrandPatch, CharacterAssets, CharacterPatch, PassportAssets,
    PersonaAssets, PersonaPatch,
};
pub use digest::VersionDigest;
pub use error::PassportError;
pub use ids::{new_generation_id, new_passport_id};
pub use kind::PassportKind;
pub use passport::{CreatePassport, Passport, PassportFilter, PassportView};
pub use ports::{
    CreateOutcome, MintMeter, MintMeterRequest, MintMeterResult, PassportStore,
    RelayConditioningSink, RelayGenerationRequest, RelayGenerationResult,
};
pub use rights::{ConsentBasis, DisclosureLevel, Rights};
pub use version::{PassportVersion, VersionStatus};

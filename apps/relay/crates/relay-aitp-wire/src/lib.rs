//! Relay-owned AITP payment wire vocabulary and cryptographic verification.
//!
//! This crate deliberately contains no HTTP client and no dependency on any
//! other Railyard application. Relay's gateway, billing engine, and durable
//! storage can therefore build and verify money evidence independently.

mod attestation;
pub mod codec;
mod error;
mod types;

pub use attestation::{
    HOLD_SETTLEMENT_ATTESTATION_FORMAT, HoldAttestationVerifyError, HoldSettlementAttestation,
    HoldSettlementAttestationClaims, HoldSettlementCommand, HoldSettlementExpectation,
    HoldSettlementOperation, hold_commitment, hold_operation_hash, hold_party_commitment,
    hold_reference_hash, no_payee_commitment,
};
pub use error::WireError;
/// Compatibility name retained for Relay callers migrating from the earlier
/// cross-application protocol crate.
pub type PayError = WireError;
pub use types::{
    AmountMicros, HoldRef, MICROS_PER_AIT, PaymentAuthorization, PaymentChallenge, Receipt, Scheme,
};

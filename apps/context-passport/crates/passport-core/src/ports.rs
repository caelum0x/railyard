//! Injectable ports. Concrete adapters (Postgres store, reqwest Relay/Mint
//! clients) live in sibling crates; tests inject fakes. Nothing in the kernel
//! depends on a database or HTTP.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PassportError;
use crate::passport::{Passport, PassportFilter};
use crate::version::PassportVersion;

/// Outcome of an idempotent create: whether this call inserted the passport,
/// plus the head version it created (or found on replay).
pub struct CreateOutcome {
    pub passport: Passport,
    pub head: PassportVersion,
    pub created: bool,
}

/// Persistence port. Implemented by the in-memory and Postgres stores.
#[async_trait]
pub trait PassportStore: Send + Sync {
    async fn create(
        &self,
        idempotency_key: &str,
        passport: &Passport,
        head: &PassportVersion,
    ) -> Result<CreateOutcome, PassportError>;

    async fn get_passport(&self, id: &str) -> Result<Option<Passport>, PassportError>;

    async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<PassportVersion>, PassportError>;

    /// The head (highest-numbered) version, draft or locked.
    async fn head_version(&self, id: &str) -> Result<Option<PassportVersion>, PassportError>;

    /// The highest-numbered *locked* version, used by `resolve`.
    async fn latest_locked_version(
        &self,
        id: &str,
    ) -> Result<Option<PassportVersion>, PassportError>;

    /// Persist an updated passport head plus a version snapshot (insert or update).
    async fn upsert(
        &self,
        passport: &Passport,
        version: &PassportVersion,
    ) -> Result<(), PassportError>;

    async fn list_passports(&self, filter: &PassportFilter)
        -> Result<Vec<Passport>, PassportError>;

    async fn ready(&self) -> Result<(), PassportError>;
}

/// A resolved conditioning request handed to Relay for actual generation.
/// `payload` is the provider-neutral conditioning as opaque JSON (the concrete
/// `ConditioningPayload` type lives in `passport-resolve`), so the kernel does
/// not depend on the resolver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayGenerationRequest {
    /// Principal that owns and pays for this conditioned generation.
    pub principal: String,
    pub passport_id: String,
    pub version: i64,
    pub provider: String,
    pub modality: String,
    pub payload: serde_json::Value,
}

/// The result of a Relay generation: a reference to the produced asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayGenerationResult {
    pub output_ref: String,
    /// Optional content hash of the produced asset (for provenance hard-binding).
    #[serde(default)]
    pub asset_hash: Option<String>,
    /// Correlated, signed settlement evidence returned by Relay/Mint.
    #[serde(default)]
    pub payment_evidence: Option<PaymentEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReceiptProof {
    pub sequence: u64,
    pub operation: String,
    pub operation_hash: String,
    pub amount_micros: i64,
    pub receipt_hash: String,
    pub signature: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentEvidence {
    pub request_id: String,
    pub payer_account: String,
    pub receipt: SettlementReceiptProof,
}

/// Transport seam to Relay (the provider fan-out gateway). Real reqwest impl in
/// `passport-relay-client`; `FakeRelay` in `passport-testkit`.
#[async_trait]
pub trait RelayConditioningSink: Send + Sync {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError>;
}

/// A success-only metering call for a passport-conditioned generation (ADR-0002).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintMeterRequest {
    /// Stable idempotency/correlation key (the generation id).
    pub reference: String,
    pub passport_id: String,
    pub version: i64,
    pub provider: String,
    pub modality: String,
}

/// The result of metering: a Mint receipt reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintMeterResult {
    pub receipt_ref: Option<String>,
}

/// Transport seam to Mint (settlement). Real reqwest impl in
/// `passport-mint-client`; `FakeMint` in `passport-testkit`. Metering is
/// best-effort and never blocks recording provenance of a successful generation.
#[async_trait]
pub trait MintMeter: Send + Sync {
    async fn meter(&self, request: &MintMeterRequest) -> Result<MintMeterResult, PassportError>;
}

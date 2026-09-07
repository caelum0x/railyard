//! Stable, gateway-independent public contract for Relay.
//!
//! Railyard applications depend on this crate instead of TensorZero gateway
//! internals. The types here are deliberately transport-shaped, versioned, and
//! free of database or provider dependencies.

pub mod aitp;
pub mod error;
pub mod headers;
pub mod model;
pub mod money;
pub mod settlement;
pub mod status;
pub mod usage;

pub use aitp::{AitpChallenge, AitpChallengeDecodeError, AitpScheme, PaymentRequired};
pub use error::{ApiError, ApiErrorEnvelope, DataEnvelope, SettledReplay};
pub use model::{ModelSelector, ModelSelectorError, OPENROUTER_CATALOG_PREFIX};
pub use money::{MICROS_PER_USD, Microdollars, MoneyError};
pub use settlement::{SettlementEvidence, SettlementReceiptProof};
pub use status::{GatewayHealth, GatewayStatus};
pub use usage::{Usage, UsageParseError};

/// Current major version of the Relay-owned public contract.
pub const CONTRACT_VERSION: u16 = 1;

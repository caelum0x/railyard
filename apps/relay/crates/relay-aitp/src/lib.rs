//! Relay's server-side AITP lifecycle.
//!
//! Mint remains the authority for balances and signature verification. This
//! crate owns Relay's complementary guarantees: challenge issuance, expiry,
//! authenticated-principal ownership, correlation, and replay-safe state.

mod bridge;
mod clock;
mod error;
mod issuer;
mod settlement;
mod store;

pub use aitp_client;
pub use bridge::{from_client_challenge, to_client_challenge};
pub use clock::{Clock, NonceSource, SystemClock, UuidNonceSource};
pub use error::{AitpError, Result};
pub use issuer::{ChallengeIssuer, ChallengePolicy};
pub use settlement::MintAttestationValidator;
pub use store::{ChallengeRecord, ChallengeState, ChallengeStore, InMemoryChallengeStore};

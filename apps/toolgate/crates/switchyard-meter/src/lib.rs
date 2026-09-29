//! # switchyard-meter
//!
//! - [`mint`]: the [`mint::MintTransport`] seam and its HTTP client for the
//!   live mint-server (`/v1/holds`, capture, release, balance, Dodo checkout).
//! - [`rago`]: the [`rago::RagoTransport`] seam and client (`POST /api/v1/events`,
//!   `transaction_id = call_id`, so redelivery is idempotent).
//! - [`outbox`]: drains durable outbox rows into Rago with backoff.
//! - [`caps`]: per-project monthly/daily, per-key daily, per-tool daily and
//!   daily-send caps, counting in-flight reservations so concurrent calls cannot
//!   jointly overshoot a cap.

pub mod caps;
pub mod mint;
pub mod outbox;
pub mod rago;

pub use caps::{CapGuard, CapScope, Caps};
pub use mint::{MintBalance, MintClient, MintError, MintHold, MintReceipt, MintTransport};
pub use outbox::OutboxWorker;
pub use rago::{RagoClient, RagoError, RagoTransport};

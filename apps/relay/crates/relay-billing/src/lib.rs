//! relay-billing: clean-room, success-only microdollar billing, ported into the
//! TensorZero (`apps/relay`) workspace as the differentiating spend engine.
//!
//! This is a self-contained consolidation of the clean-room `relay-core`,
//! `relay-store`, and `relay-billing` clean-room crates into ONE
//! crate so the graft adds a single workspace member with zero coupling to
//! TensorZero internals. Layers:
//!
//! * [`money`] — the [`Micros`] integer-microdollar newtype (ADR-0001: no float
//!   in the money path; all arithmetic checked).
//! * [`domain`] / [`usage`] / [`error`] — frozen shared vocabulary.
//! * [`store`] — the async [`Store`] trait (the ONE persistence seam) plus a
//!   pure [`InMemoryStore`] test double. TensorZero later supplies a
//!   Postgres/ClickHouse-backed impl behind the same trait.
//! * [`ledger`] — the [`Ledger`] reserve -> settle/release engine (ADR-0002:
//!   success-only; settle is the ONLY debit path; failure nets exactly zero).

pub mod backend;
pub mod domain;
pub mod error;
pub mod ledger;
pub mod mint;
pub mod money;
pub mod pricing;
pub mod store;
pub mod usage;

// Flat re-exports for ergonomic downstream `use relay_billing::...`.
pub use backend::BillingLedger;
pub use domain::{AccountId, ChatMessage, JobId, KeyId, ModelId, Role, OPENROUTER_PREFIX};
pub use error::{BillingError, ProviderError, RelayError, Result};
pub use ledger::{cost_micros, Ledger, Reservation};
pub use mint::transport::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, TransportError,
};
pub use mint::MintLedger;
pub use money::{usd_to_micros, Micros, MICROS_PER_USD};
pub use pricing::{ModelPrice, PriceTable};
pub use store::{
    now_ms, InMemoryStore, KeyStore, LedgerEntry, LedgerKind, LedgerStore, ReserveOutcome, Store,
    StoreError, StoredApiKey, Timestamp, UsageRecord, UsageStore,
};
pub use usage::Usage;

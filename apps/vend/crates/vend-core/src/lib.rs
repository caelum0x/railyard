//! # vend-core — the Vend domain kernel (pure logic, NO I/O).
//!
//! This crate is the dependency root of the Vend graph. It owns:
//! - integer-µ$ money math + the `MarkupPolicy`/`SpendCap`/`FundingMode` value
//!   types ([`money`]),
//! - the ULID id newtypes ([`ids`]),
//! - the [`VendError`] enum + `{success,data}`/`{success,error}` envelope
//!   ([`error`], [`envelope`]),
//! - the shared domain records ([`records`]),
//! - the injectable transport TRAITS ([`transport`]) and repo PORT traits
//!   ([`ports`]) — so no other crate has to depend on HTTP or a DB driver.
//!
//! Nothing here performs I/O; the concrete HTTP clients live in
//! `vend-mint-client`/`vend-relay-client` and the Postgres impls in `vend-store`.

pub mod envelope;
pub mod error;
pub mod ids;
pub mod money;
pub mod payout;
pub mod ports;
pub mod records;
pub mod transport;

pub use envelope::{ErrorBody, ErrorEnvelope, SuccessEnvelope};
pub use error::VendError;
pub use ids::{AppId, ChargeId, EndUserId, SubKeyId, TenantId};
pub use money::{price, Charge, FundingMode, MarkupPolicy, Micros, SpendCap};
pub use payout::{PayoutExecution, PayoutLeg, PayoutRepo, PayoutState};
pub use ports::{
    IdempotencyClaim, IdempotencyRepo, MeteredUsage, ReadinessRepo, ReserveRepo, UsageOutcome,
    UsageRepo,
};
pub use records::{KeyScope, KeyStatus, SubKeyRecord};
pub use transport::{
    parse_usage, CaptureRequest, CaptureResponse, CheckoutRequest, CheckoutResponse, HoldRequest,
    HoldResponse, HoldStatus, HoldView, MintBalance, MintTransport, RelayByteStream, RelayRequest,
    RelayResponse, RelayStreamResponse, RelayTransport, ReleaseRequest, ReleaseResponse,
    TransferRequest, TransferResponse, Usage,
};

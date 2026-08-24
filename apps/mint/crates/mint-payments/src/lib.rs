//! `mint-payments` — the ONLY crate that talks to provider HTTP APIs and the
//! ONLY crate that knows provider signature schemes. `mint-reserve` (the [`Ramp`]
//! trait), `mint-server` (routing/mint) and `mint-store` stay provider-agnostic.
//!
//! Three provider groups, each a folder of small adapters:
//! - [`card`] — Stripe, Creem, Polar, PayPal (card / merchant-of-record);
//! - [`iap`] — RevenueCat, Adapty (mobile IAP, webhook-only);
//! - [`crypto_tr`] — USDC (on-chain), Iyzico (Turkish card).
//!
//! Each adapter is a REAL client: real HTTP calls (via the injectable
//! [`HttpTransport`]) + real webhook signature verification (the [`verify`]
//! families, operating on RAW bytes before any serde parse). Every provider file
//! exposes `XRamp` (impl [`Ramp`]), a non-trait `create_checkout` where an
//! on-ramp checkout exists, and `verify_webhook(cfg, headers, raw)`.
//!
//! Config is env-key gated (BYOK): an unset `api_key` ⇒ the provider is inactive
//! and dropped from the [`PaymentRegistry`]; an unset `webhook_secret` ⇒ the
//! route returns `"provider not configured"`. With nothing configured, the
//! registry's ramp is a deterministic [`mint_reserve::FakeRamp`], preserving the
//! server's current dev/test behavior.

pub mod card;
pub mod composite;
pub mod config;
pub mod crypto_tr;
pub mod error;
pub mod event;
pub mod http;
pub mod iap;
mod money;
pub mod provider;
pub mod registry;
pub mod verify;

pub use composite::CompositeRamp;
pub use config::{Mode, PaymentsConfig, ProviderConfig};
pub use error::PaymentError;
pub use event::{DepositEvent, WebhookError};
pub use http::{Headers, HttpResponse, HttpTransport, ReqwestTransport, TransportError};
pub use provider::ProviderId;
pub use registry::PaymentRegistry;

// Re-export the ramp vocabulary so `mint-server` can wire the registry without
// depending on `mint-reserve` internals directly.
pub use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};

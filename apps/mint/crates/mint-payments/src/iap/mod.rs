//! Mobile IAP aggregators: RevenueCat, Adapty.
//!
//! These are webhook-ONLY on-ramps (the purchase happens SDK-side, so there is
//! no server checkout route). Webhook authenticity is an `Authorization`
//! shared-secret compared in CONSTANT TIME (via `subtle`), NOT an HMAC. Neither
//! provider exposes an arbitrary-payout API ⇒ `initiate_payout` returns
//! `ProviderUnavailable`.

pub mod adapty;
pub mod revenuecat;

pub use adapty::AdaptyRamp;
pub use revenuecat::RevenueCatRamp;

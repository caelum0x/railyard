//! Card / merchant-of-record on-ramps: Stripe, Creem, Polar, PayPal.
//!
//! Each provider file exposes a `XRamp` implementing the [`mint_reserve::Ramp`]
//! trait (real `confirm_deposit` re-confirmation + `initiate_payout` where the
//! provider supports arbitrary payouts), a non-trait `create_checkout` helper,
//! and a `verify_webhook(cfg, headers, raw)` that runs signature verification on
//! RAW bytes before any serde parse.

pub mod creem;
pub mod paypal;
pub mod polar;
pub mod stripe;

pub use creem::CreemRamp;
pub use paypal::PayPalRamp;
pub use polar::PolarRamp;
pub use stripe::StripeRamp;

/// A hosted-checkout creation result: the URL to redirect the buyer to plus the
/// provider's checkout/session id (used to correlate the later webhook).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    /// The provider-hosted checkout URL.
    pub checkout_url: String,
    /// The provider's checkout/session/order id.
    pub checkout_id: String,
}

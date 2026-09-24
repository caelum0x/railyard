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

/// Where the provider sends the buyer after a hosted checkout. Resolved by
/// `mint-server` from the request, then `MINT_CHECKOUT_SUCCESS_URL` /
/// `MINT_CHECKOUT_CANCEL_URL`, then Mint's own hosted `/checkout/*` pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutUrls {
    /// Buyer lands here after paying.
    pub success_url: String,
    /// Buyer lands here after abandoning the checkout.
    pub cancel_url: String,
    /// Provider-approval return target for providers that need a server-side
    /// capture after the buyer approves (PayPal): Mint's own
    /// `/checkout/paypal/return` endpoint, which captures and then redirects to
    /// `success_url`. `None` when Mint's public URL is not configured.
    pub capture_return_url: Option<String>,
    /// Mint's own public base URL (`MINT_PUBLIC_URL`, no trailing slash), for
    /// providers whose hosted checkout returns the buyer's browser to Mint with
    /// a form POST (Iyzico → `{base}/v1/deposits/iyzico/webhook`). `None` when
    /// Mint's public URL is not configured.
    pub public_base_url: Option<String>,
}

/// `true` iff an amount in microdollars is a whole number of cents (every fiat
/// rail pays in cents; a sub-cent remainder would be burned but never paid).
pub(crate) fn is_whole_cents(amount_micros: i64) -> bool {
    amount_micros > 0 && amount_micros % 10_000 == 0
}

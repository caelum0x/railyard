//! Seller-own payments (Wave P PY2.3, VC6; contract 5.5 of the Wave P
//! dispatch): a Scribase customer (or Menivor, MV13) sells through **their
//! own** payment account, and Mint is only the connector.
//!
//! - The seller connects their own Dodo account on Mint's connect page
//!   (Advanced; the key is typed there only, never in the calling product),
//!   or their Stripe account through Stripe Connect OAuth when the operator
//!   has switched it on (owner decision OD-9).
//! - Products and checkout sessions are created **on the seller's account**;
//!   the buyer pays the seller's processor directly.
//! - The seller's processor webhooks become entitlement events (`granted`,
//!   `revoked` for a refund, dispute, cancellation or expiry) that Mint
//!   forwards, Standard Webhooks signed, to the calling product.
//!
//! **Invariant:** nothing here has a handle on the Mint ledger. Mint never
//! receives, holds or forwards a buyer's money: no deposit, no balance, no
//! payout is created by this module (tested in mint-server).

pub mod dodo;
pub mod seal;
pub mod stripe;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Whose processor the seller connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SellerProvider {
    Dodo,
    Stripe,
}

impl SellerProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dodo => "dodo",
            Self::Stripe => "stripe",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "dodo" => Some(Self::Dodo),
            "stripe" => Some(Self::Stripe),
            _ => None,
        }
    }
}

/// Test or live keys on the seller's account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SellerMode {
    Test,
    Live,
}

impl SellerMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Live => "live",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "test" => Some(Self::Test),
            "live" => Some(Self::Live),
            _ => None,
        }
    }
}

/// The seller's connected account, opened from its sealed form for one call.
/// `Debug` never shows the secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct SellerAccount {
    pub provider: SellerProvider,
    pub mode: SellerMode,
    /// Dodo: the seller's API key. Stripe: the connected account id (`acct_...`).
    pub secret: String,
    /// Dodo: the seller's webhook secret (`whsec_...`). Stripe: unused (the
    /// platform's Connect endpoint secret verifies every account's events).
    pub webhook_secret: Option<String>,
}

impl fmt::Debug for SellerAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SellerAccount")
            .field("provider", &self.provider)
            .field("mode", &self.mode)
            .field("secret", &"[sealed]")
            .finish_non_exhaustive()
    }
}

/// How often a product renews; `None` is a one-time purchase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Month,
    Year,
}

/// A product the seller sells (created on the seller's own account).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductSpec {
    /// The seller's own id for it; the entitlement name in their app.
    pub sku: String,
    pub name: String,
    /// Whole cents of `currency`.
    pub price_cents: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    pub interval: Option<Interval>,
}

impl ProductSpec {
    /// Checks the fields before anything is sent to a processor.
    ///
    /// # Errors
    /// A plain reason when a field cannot be used.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_sku(&self.sku) {
            return Err("sku must be 1-64 lowercase letters, digits, dots, dashes or underscores".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            return Err("name must be 1-120 characters".into());
        }
        if !(50..=100_000_000).contains(&self.price_cents) {
            return Err("price_cents must be between 50 and 100000000".into());
        }
        if self.currency.len() != 3 || !self.currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err("currency must be an ISO code such as USD".into());
        }
        Ok(())
    }
}

/// A seller-side id the calling product chooses (`org.project.env`, ...).
pub fn valid_seller(value: &str) -> bool {
    (1..=120).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn valid_sku(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// The app's own user id for the buyer (what the entitlement is granted to).
pub fn valid_buyer_ref(value: &str) -> bool {
    (1..=200).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic())
}

/// Percent or a fixed amount off (Wave Q, for QM1.3 creator coupons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscountKind {
    Percentage,
    Flat,
}

/// A discount code made on the seller's own account, in the shape Dodo's
/// `POST /discounts` takes. Mint keeps nothing of it: the code lives on the
/// seller's processor and the calling product keeps only the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscountSpec {
    /// 3-32 characters, upper-cased (the processor upper-cases codes too).
    pub code: String,
    pub name: String,
    pub kind: DiscountKind,
    /// Basis points for a percentage (1-10000); the smallest currency unit for flat.
    pub amount: i64,
    /// ISO 4217 for a flat amount.
    pub currency: Option<String>,
    /// RFC 3339; `None` never expires.
    pub expires_at: Option<String>,
    /// The seller's processor product ids the code is limited to (empty: all).
    pub restricted_to: Vec<String>,
}

/// A code the calling product may pass to a checkout (3-32 of A-Z, 0-9, `-`, `_`).
pub fn valid_discount_code(value: &str) -> bool {
    (3..=32).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl DiscountSpec {
    /// Checks the fields before anything is sent to a processor.
    ///
    /// # Errors
    /// A plain reason when a field cannot be used.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_discount_code(&self.code) {
            return Err("code must be 3-32 letters, digits, dashes or underscores".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            return Err("name must be 1-120 characters".into());
        }
        match self.kind {
            DiscountKind::Percentage if !(1..=10_000).contains(&self.amount) => return Err("a percentage amount is in basis points, 1 to 10000".into()),
            DiscountKind::Flat if !(1..=100_000_000).contains(&self.amount) => return Err("a flat amount is 1 to 100000000 of the smallest currency unit".into()),
            _ => {}
        }
        if self.kind == DiscountKind::Flat && !self.currency.as_deref().is_some_and(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase())) {
            return Err("a flat amount needs its currency, an ISO code such as USD".into());
        }
        if let Some(at) = &self.expires_at {
            if dodo::parse_rfc3339_ms(at).is_none() {
                return Err("expires_at must be an RFC 3339 date and time".into());
            }
        }
        if self.restricted_to.len() > 50 {
            return Err("a code can be limited to at most 50 products".into());
        }
        Ok(())
    }
}

/// A discount as the seller's processor made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Discount {
    pub id: String,
    pub code: String,
}

/// A discount as the seller's processor holds it now (read back after a
/// change or a lookup).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscountState {
    pub id: String,
    pub code: String,
    pub kind: DiscountKind,
    pub amount: i64,
    pub expires_at: Option<String>,
}

/// A discount id the processor issued (`dsc_...`); checked before it goes
/// into a processor URL.
pub fn valid_discount_id(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A partial change to one discount, in the shape Dodo's
/// `PATCH /discounts/{discount_id}` takes. `None` leaves a field as it is.
///
/// `kind` and `amount` travel together (the amount means basis points or
/// money depending on the type). A flat change also names its currency; a
/// change to a percentage clears the old currency rows, so a former flat
/// amount never lingers as a percentage cap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscountPatch {
    pub code: Option<String>,
    pub name: Option<String>,
    pub kind: Option<DiscountKind>,
    pub amount: Option<i64>,
    /// ISO 4217; only with a flat `kind`.
    pub currency: Option<String>,
    /// `Some(None)` clears the expiry; `Some(Some(t))` sets it.
    pub expires_at: Option<Option<String>>,
    /// The seller's processor product ids (replaces the set; empty clears it).
    pub restricted_to: Option<Vec<String>>,
}

impl DiscountPatch {
    /// Checks the fields before anything is sent to a processor.
    ///
    /// # Errors
    /// A plain reason when a field cannot be used.
    pub fn validate(&self) -> Result<(), String> {
        if *self == Self::default() {
            return Err("nothing to change on this discount".into());
        }
        if self.code.as_deref().is_some_and(|c| !valid_discount_code(c)) {
            return Err("code must be 3-32 letters, digits, dashes or underscores".into());
        }
        if self.name.as_deref().is_some_and(|n| n.trim().is_empty() || n.chars().count() > 120) {
            return Err("name must be 1-120 characters".into());
        }
        match (self.kind, self.amount) {
            (None, None) => {}
            (Some(DiscountKind::Percentage), Some(a)) if (1..=10_000).contains(&a) => {}
            (Some(DiscountKind::Percentage), Some(_)) => return Err("a percentage amount is in basis points, 1 to 10000".into()),
            (Some(DiscountKind::Flat), Some(a)) if (1..=100_000_000).contains(&a) => {}
            (Some(DiscountKind::Flat), Some(_)) => return Err("a flat amount is 1 to 100000000 of the smallest currency unit".into()),
            _ => return Err("type and amount change together".into()),
        }
        match (self.kind, self.currency.as_deref()) {
            (Some(DiscountKind::Flat), Some(c)) if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) => {}
            (Some(DiscountKind::Flat), _) => return Err("a flat amount needs its currency, an ISO code such as USD".into()),
            (_, Some(_)) => return Err("a currency goes with a flat amount only".into()),
            _ => {}
        }
        if let Some(Some(at)) = &self.expires_at {
            if dodo::parse_rfc3339_ms(at).is_none() {
                return Err("expires_at must be an RFC 3339 date and time".into());
            }
        }
        if self.restricted_to.as_ref().is_some_and(|r| r.len() > 50) {
            return Err("a code can be limited to at most 50 products".into());
        }
        Ok(())
    }
}

/// A hosted checkout on the seller's account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckoutSession {
    pub url: String,
    pub id: String,
}

/// The metadata every checkout carries so its webhooks map back to a buyer
/// and a product of one seller. Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutMeta<'a> {
    /// `<product>:<seller>` so one Mint serves every calling product.
    pub seller_key: &'a str,
    pub buyer_ref: &'a str,
    pub sku: &'a str,
}

/// What a verified seller webhook means for the buyer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntitlementKind {
    Granted,
    Revoked,
}

impl EntitlementKind {
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::Granted => "entitlement.granted",
            Self::Revoked => "entitlement.revoked",
        }
    }
}

/// Why an entitlement ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RevokeReason {
    Refund,
    Dispute,
    Cancelled,
    Expired,
}

impl RevokeReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refund => "refund",
            Self::Dispute => "dispute",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

/// One entitlement change from a seller's processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementEvent {
    pub kind: EntitlementKind,
    pub reason: Option<RevokeReason>,
    pub seller_key: String,
    pub buyer_ref: String,
    pub sku: String,
    /// The processor's event (or payment) id; unique per provider.
    pub provider_event_id: String,
    pub occurred_at_ms: i64,
    /// When a subscription period ends (renewals), if the processor said.
    pub expires_at_ms: Option<i64>,
}

/// Why a seller call failed (never carries a secret).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SellerError {
    #[error("{0}")]
    Invalid(String),
    #[error("the payment account refused the request: {0}")]
    Refused(String),
    #[error("the payment account did not answer")]
    Unavailable,
    #[error("this seller has not connected a payment account")]
    NotConnected,
    /// The processor has no such object on this seller's account (a
    /// discount deleted on the processor's dashboard, say).
    #[error("the payment account has no such object")]
    NotFound,
}

pub(crate) fn meta_from(value: &serde_json::Value) -> Option<(String, String, String)> {
    let text = |k: &str| value.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let seller = text("mint_seller")?;
    let buyer = text("buyer_ref")?;
    let sku = text("sku")?;
    (valid_buyer_ref(&buyer) && valid_sku(&sku) && !seller.is_empty()).then_some((seller, buyer, sku))
}

#[cfg(test)]
mod tests {
    use super::{valid_buyer_ref, valid_seller, valid_sku, Interval, ProductSpec, SellerAccount, SellerMode, SellerProvider};

    #[test]
    fn ids_and_products_are_checked_before_any_processor_call() {
        assert!(valid_seller("org_1.shop.production") && !valid_seller(".x") && !valid_seller("a b"));
        assert!(valid_sku("pro.monthly") && !valid_sku("Pro"));
        assert!(valid_buyer_ref("user_123") && !valid_buyer_ref("a b"));
        let ok = ProductSpec { sku: "pro".into(), name: "Pro".into(), price_cents: 500, currency: "USD".into(), interval: Some(Interval::Month) };
        assert!(ok.validate().is_ok());
        assert!(ProductSpec { price_cents: 10, ..ok.clone() }.validate().is_err());
        assert!(ProductSpec { currency: "usd".into(), ..ok.clone() }.validate().is_err());
        assert!(ProductSpec { sku: "Bad Sku".into(), ..ok }.validate().is_err());
    }

    #[test]
    fn debug_never_shows_the_seller_secret() {
        let acct = SellerAccount { provider: SellerProvider::Dodo, mode: SellerMode::Test, secret: "sk_test_secret_value".into(), webhook_secret: Some("whsec_x".into()) };
        let shown = format!("{acct:?}");
        assert!(!shown.contains("sk_test_secret_value") && !shown.contains("whsec_x"));
    }
}

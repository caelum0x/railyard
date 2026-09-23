//! Boundary DTOs: `serde` request bodies (§4) and response projections for the
//! domain records that do not themselves implement `Serialize`. Every amount is
//! a bare `i64` µ$ — never a float. Typed ids arrive as strings and are parsed
//! at the boundary (never trusted raw).

use mint_core::{AccountId, AccountKind, Micros};
use mint_reserve::{KycTier, PayoutDest};
use mint_store::{
    AllowanceRecord, AllowanceScheme, AllowanceScope, AllowanceWindow, HoldRecord, HoldStatus,
    StreamRecord, StreamStatus, StreamUnit,
};
use serde::{Deserialize, Serialize};

// ---- requests --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateAccountReq {
    pub kind: AccountKind,
    #[serde(default)]
    pub owner_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAddressReq {
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransferReq {
    pub from: String,
    pub to: String,
    pub amount_micros: i64,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HoldCreateReq {
    pub from: String,
    pub amount_micros: i64,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HoldCaptureReq {
    pub amount_micros: i64,
    pub to: String,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HoldReleaseReq {
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamOpenReq {
    pub from: String,
    pub to: String,
    pub rate_micros: i64,
    pub cap_micros: i64,
    pub unit: StreamUnit,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamTickReq {
    pub units: u64,
    pub seq: u64,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamCloseReq {
    #[serde(default)]
    pub units: u64,
    #[serde(default)]
    pub seq: u64,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AllowanceGrantReq {
    pub granter: String,
    pub to_key: String,
    pub limit_micros: i64,
    pub window: AllowanceWindow,
    #[serde(default)]
    pub scope: Option<AllowanceScope>,
    pub idem_key: String,
}

#[derive(Debug, Deserialize)]
pub struct AllowanceSpendReq {
    pub amount_micros: i64,
    pub to: String,
    pub scheme: AllowanceScheme,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
    pub idem_key: String,
}

#[derive(Debug, Deserialize)]
pub struct AllowanceRevokeReq {
    pub idem_key: String,
}

#[derive(Debug, Deserialize)]
pub struct AitpChallengeReq {
    pub payee: String,
    pub amount_micros: i64,
    pub resource: String,
    pub scheme: mint_protocol::Scheme,
    pub ttl_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct AitpCaptureReq {
    pub hold_id: String,
    pub amount_micros: i64,
    pub to: String,
    pub idem_key: String,
}

#[derive(Debug, Deserialize)]
pub struct AitpReleaseReq {
    pub hold_id: String,
    pub idem_key: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutReq {
    pub provider: String,
    pub account_id: String,
    pub amount_micros: i64,
    /// Where the buyer lands after paying (https). Defaults to
    /// `MINT_CHECKOUT_SUCCESS_URL`, then Mint's hosted `/checkout/success`.
    #[serde(default)]
    pub success_url: Option<String>,
    /// Where the buyer lands after abandoning the checkout (https).
    #[serde(default)]
    pub cancel_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MintReq {
    pub to: String,
    pub amount_micros: i64,
    pub deposit_ref: String,
    #[serde(default)]
    pub idem_key: String,
}

/// The presented KYC tier (wire form; mapped to [`KycTier`]).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KycTierDto {
    None,
    Basic,
    Verified,
}

impl From<KycTierDto> for KycTier {
    fn from(v: KycTierDto) -> Self {
        match v {
            KycTierDto::None => KycTier::None,
            KycTierDto::Basic => KycTier::Basic,
            KycTierDto::Verified => KycTier::Verified,
        }
    }
}

/// Where a payout is sent (wire form; mapped to [`PayoutDest`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PayoutDestDto {
    Stripe { account: String },
    Crypto { chain: String, address: String },
    Paypal { email: String },
}

impl From<PayoutDestDto> for PayoutDest {
    fn from(v: PayoutDestDto) -> Self {
        match v {
            PayoutDestDto::Stripe { account } => PayoutDest::Stripe { account },
            PayoutDestDto::Crypto { chain, address } => PayoutDest::Crypto { chain, address },
            PayoutDestDto::Paypal { email } => PayoutDest::PayPal { email },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RedeemReq {
    pub from: String,
    pub amount_micros: i64,
    pub payout_ref: String,
    pub kyc_tier: KycTierDto,
    pub payout_dest: PayoutDestDto,
    #[serde(default)]
    pub idem_key: String,
}

// ---- response projections --------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AccountView {
    pub id: String,
    pub kind: AccountKind,
    pub status: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HoldView {
    pub id: String,
    pub account_id: String,
    pub amount_micros: i64,
    pub captured_micros: i64,
    pub status: HoldStatus,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

impl From<HoldRecord> for HoldView {
    fn from(r: HoldRecord) -> Self {
        HoldView {
            id: r.id.as_str().to_string(),
            account_id: r.account_id.as_str().to_string(),
            amount_micros: r.amount_micros.get(),
            captured_micros: r.captured_micros.get(),
            status: r.status,
            ref_: r.ref_,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StreamView {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub rate_micros: i64,
    pub unit: StreamUnit,
    pub cap_micros: i64,
    pub settled_micros: i64,
    pub last_seq: u64,
    pub status: StreamStatus,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

impl From<StreamRecord> for StreamView {
    fn from(r: StreamRecord) -> Self {
        StreamView {
            id: r.id.as_str().to_string(),
            from_id: r.from_id.as_str().to_string(),
            to_id: r.to_id.as_str().to_string(),
            rate_micros: r.rate_micros.get(),
            unit: r.unit,
            cap_micros: r.cap_micros.get(),
            settled_micros: r.settled_micros.get(),
            last_seq: r.last_seq,
            status: r.status,
            ref_: r.ref_,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AllowanceView {
    pub id: String,
    pub granter: String,
    pub to_key: String,
    pub limit_micros: i64,
    pub spent_micros: i64,
    pub window: AllowanceWindow,
    pub window_start_ms: i64,
    pub scope: Option<AllowanceScope>,
    pub revoked: bool,
}

impl From<AllowanceRecord> for AllowanceView {
    fn from(r: AllowanceRecord) -> Self {
        AllowanceView {
            id: r.id.as_str().to_string(),
            granter: r.granter.as_str().to_string(),
            to_key: r.to_key,
            limit_micros: r.limit_micros.get(),
            spent_micros: r.spent_micros.get(),
            window: r.window,
            window_start_ms: r.window_start_ms,
            scope: r.scope,
            revoked: r.revoked,
        }
    }
}

/// Parse a raw account-id string at the boundary (never trust it as valid).
pub fn parse_account(s: &str) -> Result<AccountId, mint_core::MintError> {
    AccountId::parse(s)
}

/// A µ$ amount echoed back as a bare integer.
pub fn micros(v: i64) -> Micros {
    Micros::new(v)
}

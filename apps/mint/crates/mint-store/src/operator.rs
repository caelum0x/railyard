//! Operator-plane vocabulary: account lifecycle (freeze / close), DB-backed
//! API keys, imported provider settlement reports (for reconciliation against
//! the provider's own books) and deposit reversals (refund / chargeback
//! claw-backs).
//!
//! None of these records are money: they never touch the journal, receipts or
//! reserve. Account status is consulted by the server's store wrapper BEFORE a
//! money path runs; everything else is operator bookkeeping.

use serde::{Deserialize, Serialize};

/// Lifecycle state of an account (the SQL `accounts.status` smallint).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// Normal operation.
    #[default]
    Active,
    /// Operator freeze: every OUTGOING movement (transfer, hold, stream,
    /// allowance, AITP settlement, redeem) is refused with `account_frozen`.
    /// Incoming credits still land so a customer's deposit is never lost.
    Frozen,
    /// Terminal: no movement in or out (`account_closed`). Only an account with
    /// a zero available AND held balance can be closed.
    Closed,
}

impl AccountStatus {
    /// Column value (`accounts.status`).
    pub const fn as_i16(self) -> i16 {
        match self {
            AccountStatus::Active => 0,
            AccountStatus::Frozen => 1,
            AccountStatus::Closed => 2,
        }
    }

    /// Decode the column value (unknown values are treated as frozen — the
    /// fail-safe for money movement).
    pub const fn from_i16(v: i16) -> Self {
        match v {
            0 => AccountStatus::Active,
            2 => AccountStatus::Closed,
            _ => AccountStatus::Frozen,
        }
    }

    /// Stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            AccountStatus::Active => "active",
            AccountStatus::Frozen => "frozen",
            AccountStatus::Closed => "closed",
        }
    }
}

/// A DB-backed API key. Only the SHA-256 of the secret is stored; the secret
/// itself is shown to the operator exactly once, at creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Stable id (`key_<ULID>`).
    pub id: String,
    /// Operator-facing label.
    pub name: String,
    /// Lowercase hex SHA-256 of the secret.
    pub key_hash: String,
    /// The first characters of the secret, for recognition in listings.
    pub prefix: String,
    /// Granted scopes (`read`, `accounts`, `spend`, `funding`, `payouts`, `admin`).
    pub scopes: Vec<String>,
    /// When set, the key may only act on (and read) these account ids.
    pub accounts: Option<Vec<String>>,
    /// Creation time (unix ms).
    pub created_ms: i64,
    /// Revocation time (unix ms); a revoked key never authenticates.
    pub revoked_ms: Option<i64>,
}

/// A registered AITP-402 payer verifying key (Ed25519). Agents sign
/// `AITP-Authorization`s with the matching private key; Mint only ever stores
/// the public half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerKeyRecord {
    /// The paying account (`acct_…`) the key authorizes spend for.
    pub payer: String,
    /// Caller-chosen key id echoed in `Authorization.pubkey_id`.
    pub pubkey_id: String,
    /// Lowercase hex of the 32-byte Ed25519 public key.
    pub public_key_hex: String,
    /// Operator-facing label.
    pub label: Option<String>,
    /// Registration time (unix ms).
    pub created_ms: i64,
    /// Revocation time (unix ms); a revoked key never verifies.
    pub revoked_ms: Option<i64>,
}

/// Header of one imported provider report (a Stripe balance-transaction
/// export, PayPal settlement report, on-chain transfer list …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReport {
    /// Stable id (`rpt_<ULID>`).
    pub id: String,
    /// Provider tag (`stripe`, `paypal`, `usdc` …) matching the `<provider>:`
    /// prefix of Mint's deposit / payout refs.
    pub provider: String,
    /// Operator label (usually the uploaded file name).
    pub label: Option<String>,
    /// Import time (unix ms).
    pub imported_ms: i64,
    /// Number of parsed lines.
    pub line_count: u64,
    /// Earliest line timestamp (unix ms), when the report carries dates.
    pub period_start_ms: Option<i64>,
    /// Latest line timestamp (unix ms), when the report carries dates.
    pub period_end_ms: Option<i64>,
}

/// Classification of one provider report line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportLineKind {
    /// Money received from a customer (should match a Mint mint).
    Deposit,
    /// Money sent out to a payee (should match a Mint payout).
    Payout,
    /// A refund of a prior deposit (should match a deposit reversal).
    Refund,
    /// A chargeback / dispute of a prior deposit.
    Dispute,
    /// Fees, adjustments and anything else (informational only).
    Other,
}

impl ReportLineKind {
    /// Stable wire / column string.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReportLineKind::Deposit => "deposit",
            ReportLineKind::Payout => "payout",
            ReportLineKind::Refund => "refund",
            ReportLineKind::Dispute => "dispute",
            ReportLineKind::Other => "other",
        }
    }

    /// Parse the column string.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deposit" => Some(ReportLineKind::Deposit),
            "payout" => Some(ReportLineKind::Payout),
            "refund" => Some(ReportLineKind::Refund),
            "dispute" => Some(ReportLineKind::Dispute),
            "other" => Some(ReportLineKind::Other),
            _ => None,
        }
    }
}

/// One parsed line of an imported provider report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReportLine {
    /// 1-based line number within the report.
    pub line_no: u32,
    /// Classification.
    pub kind: ReportLineKind,
    /// The provider's reference (PaymentIntent / charge / transaction id / tx
    /// hash), WITHOUT the `<provider>:` prefix.
    pub reference: String,
    /// Absolute amount in microdollars.
    pub amount_micros: i64,
    /// ISO currency (`usd`, `usdc` …) when the report states it.
    pub currency: Option<String>,
    /// Line timestamp (unix ms) when the report states it.
    pub occurred_ms: Option<i64>,
}

/// One reserve movement (a mint or a redeem burn) with its namespaced ref —
/// the Mint side of a provider-report match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReserveMovement {
    /// `"mint"` or `"redeem"`.
    pub kind: String,
    /// The full `<provider>:<ref>` deposit / payout reference.
    pub reference: String,
    /// The account credited / burned from, when known.
    pub account_id: Option<String>,
    /// Absolute amount in microdollars.
    pub amount_micros: i64,
    /// Movement time (unix ms).
    pub ts_ms: i64,
}

/// Lifecycle of a deposit reversal (refund / chargeback claw-back).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversalStatus {
    /// The full reversed amount was burned from the account.
    ClawedBack,
    /// Only part (or none) could be burned — the account had already spent the
    /// funds. The remainder is an operator worklist item.
    Shortfall,
    /// An operator wrote the shortfall off (absorbed as a loss).
    WrittenOff,
}

impl ReversalStatus {
    /// Stable wire / column string.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReversalStatus::ClawedBack => "clawed_back",
            ReversalStatus::Shortfall => "shortfall",
            ReversalStatus::WrittenOff => "written_off",
        }
    }

    /// Parse the column string.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "clawed_back" => Some(ReversalStatus::ClawedBack),
            "shortfall" => Some(ReversalStatus::Shortfall),
            "written_off" => Some(ReversalStatus::WrittenOff),
            _ => None,
        }
    }
}

/// A recorded reversal of a previously-minted deposit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositReversal {
    /// The original `<provider>:<ref>` deposit reference (primary key).
    pub deposit_ref: String,
    /// The account the deposit credited.
    pub account_id: String,
    /// Amount the provider took back.
    pub amount_micros: i64,
    /// Amount burned from the account so far.
    pub clawed_micros: i64,
    /// `refund` or `dispute`.
    pub reason: String,
    /// Lifecycle state.
    pub status: ReversalStatus,
    /// Number of claw-back burns attempted (drives the burn idempotency key).
    pub attempts: u32,
    /// Free-form operator note.
    pub note: Option<String>,
    /// Creation time (unix ms). Assigned by the store on first insert.
    pub created_ms: i64,
    /// Last update time (unix ms). Assigned by the store on every upsert.
    pub updated_ms: i64,
}

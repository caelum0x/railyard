//! The injectable fiat/crypto ramp + KYC seams (deterministic, fake-tested).
//!
//! Mirrors the `Clock`/`Signer` determinism seams: [`Ramp`] confirms inbound
//! deposits and initiates payouts; [`KycGate`] authorizes redemptions. Concrete
//! `StripeRamp`/`CryptoRamp` impls live behind DI in production — here we ship
//! deterministic [`FakeRamp`] / [`FakeKycGate`] with NO network and NO keys.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use mint_core::{AccountId, Micros, MintError};

/// The settlement asset backing a deposit / payout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asset {
    /// US dollars (Stripe fiat).
    Usd,
    /// USD Coin (crypto on-ramp).
    Usdc,
}

/// Where a payout is sent. `Stripe` for fiat, `Crypto` for on-chain.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PayoutDest {
    /// A Stripe connected-account transfer target.
    Stripe {
        /// The destination Stripe account id.
        account: String,
    },
    /// An on-chain address on a named chain.
    Crypto {
        /// The chain (e.g. `"ethereum"`, `"solana"`).
        chain: String,
        /// The destination address.
        address: String,
    },
}

/// A deposit as reported by the ramp. `mint` is gated on `confirmed == true` and
/// `amount_micros >= requested`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Deposit {
    /// The provider's deposit reference (PaymentIntent id / on-chain tx hash).
    pub deposit_ref: String,
    /// The confirmed deposit amount in microdollars.
    pub amount_micros: Micros,
    /// The asset deposited.
    pub asset: Asset,
    /// Whether the provider has confirmed the funds have landed.
    pub confirmed: bool,
    /// The account the provider says actually initiated & paid for this deposit,
    /// as read back from an AUTHENTICATED provider re-query (e.g. Iyzico
    /// `/payment/detail` `conversationId`). When `Some`, [`ReserveLedger::mint`]
    /// requires it to equal the account being credited, so a forged/unauthenticated
    /// webhook cannot redirect a real payment to an attacker-chosen account.
    /// `None` for providers whose webhook is itself authenticated end-to-end.
    pub bound_account: Option<String>,
}

/// The lifecycle of an initiated payout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PayoutStatus {
    /// The provider accepted and sent (or queued) the payout.
    Paid,
    /// Accepted but not yet settled downstream.
    Pending,
    /// The provider rejected / could not send it (retry against the same ref).
    Failed,
}

/// A payout as reported by the ramp after `initiate_payout`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Payout {
    /// The idempotency reference for this payout (redeem's `payout_ref`).
    pub payout_ref: String,
    /// The amount sent, in microdollars.
    pub amount_micros: Micros,
    /// The provider-reported status.
    pub status: PayoutStatus,
}

/// Failures the ramp can report. Deposit-side failures map to
/// [`MintError::DepositNotConfirmed`]; a post-burn payout failure is surfaced on
/// the [`crate::RedeemReceipt`] (status `Failed`) so it can be retried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RampError {
    /// No deposit exists for the given ref.
    DepositNotFound,
    /// The deposit exists but is not yet confirmed.
    NotConfirmed,
    /// The provider is transiently unavailable (retryable).
    ProviderUnavailable,
}

/// The injectable ramp seam.
#[async_trait]
pub trait Ramp: Send + Sync {
    /// Confirm an inbound deposit (before a `mint`).
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError>;

    /// Send a payout out (after a `redeem` burn). Idempotent by `payout_ref`.
    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError>;
}

/// A KYC verification tier. Ordered: `None < Basic < Verified`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum KycTier {
    /// Unverified — no redemptions permitted.
    None,
    /// Basic verification (email/phone).
    Basic,
    /// Full identity verification.
    Verified,
}

/// The injectable KYC / payout-limit gate.
#[async_trait]
pub trait KycGate: Send + Sync {
    /// Authorize a redemption of `amount` by `account` at the presented `tier`.
    /// Returns [`MintError::KycRequired`] below the required tier and
    /// [`MintError::PayoutLimitExceeded`] over the limit.
    async fn authorize(
        &self,
        account: &AccountId,
        tier: KycTier,
        amount: Micros,
    ) -> Result<(), MintError>;
}

// ---- Deterministic test fakes (no network, no keys) -----------------------

/// A deterministic [`Ramp`] fake: a preloaded map of deposits + a recorded,
/// idempotent list of initiated payouts, with a toggle to simulate a transient
/// provider outage.
pub struct FakeRamp {
    deposits: HashMap<String, Deposit>,
    payouts: Mutex<Vec<Payout>>,
    payout_should_fail: Mutex<bool>,
}

impl Default for FakeRamp {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRamp {
    /// An empty fake ramp.
    pub fn new() -> Self {
        FakeRamp {
            deposits: HashMap::new(),
            payouts: Mutex::new(Vec::new()),
            payout_should_fail: Mutex::new(false),
        }
    }

    /// Preload a confirmed deposit (builder style).
    pub fn with_confirmed_deposit(
        mut self,
        deposit_ref: impl Into<String>,
        amount_micros: i64,
        asset: Asset,
    ) -> Self {
        let deposit_ref = deposit_ref.into();
        self.deposits.insert(
            deposit_ref.clone(),
            Deposit {
                deposit_ref,
                amount_micros: Micros::new(amount_micros),
                asset,
                confirmed: true,
                bound_account: None,
            },
        );
        self
    }

    /// Preload a confirmed deposit whose provider-verified payer is `bound_account`
    /// (builder style). `mint` credits it ONLY when the target account equals
    /// `bound_account`; any other target is rejected as an unbound/forged credit.
    pub fn with_bound_deposit(
        mut self,
        deposit_ref: impl Into<String>,
        amount_micros: i64,
        asset: Asset,
        bound_account: impl Into<String>,
    ) -> Self {
        let deposit_ref = deposit_ref.into();
        self.deposits.insert(
            deposit_ref.clone(),
            Deposit {
                deposit_ref,
                amount_micros: Micros::new(amount_micros),
                asset,
                confirmed: true,
                bound_account: Some(bound_account.into()),
            },
        );
        self
    }

    /// Preload an UNconfirmed (pending) deposit (builder style).
    pub fn with_pending_deposit(
        mut self,
        deposit_ref: impl Into<String>,
        amount_micros: i64,
        asset: Asset,
    ) -> Self {
        let deposit_ref = deposit_ref.into();
        self.deposits.insert(
            deposit_ref.clone(),
            Deposit {
                deposit_ref,
                amount_micros: Micros::new(amount_micros),
                asset,
                confirmed: false,
                bound_account: None,
            },
        );
        self
    }

    /// Simulate (or clear) a transient payout outage.
    pub fn set_payout_failure(&self, should_fail: bool) {
        *self.payout_should_fail.lock().unwrap() = should_fail;
    }

    /// Snapshot the recorded payouts (assert exactly-once + amount).
    pub fn payouts(&self) -> Vec<Payout> {
        self.payouts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Ramp for FakeRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        match self.deposits.get(deposit_ref) {
            None => Err(RampError::DepositNotFound),
            Some(d) if !d.confirmed => Err(RampError::NotConfirmed),
            Some(d) => Ok(d.clone()),
        }
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        if *self.payout_should_fail.lock().unwrap() {
            return Err(RampError::ProviderUnavailable);
        }
        let mut payouts = self.payouts.lock().unwrap();
        // Idempotent by payout_ref: a retry against an already-sent ref returns
        // the recorded payout WITHOUT sending a second one.
        if let Some(existing) = payouts.iter().find(|p| p.payout_ref == payout_ref) {
            return Ok(existing.clone());
        }
        let payout = Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
        };
        payouts.push(payout.clone());
        Ok(payout)
    }
}

/// A deterministic [`KycGate`] fake: a required minimum tier + a per-tx limit.
pub struct FakeKycGate {
    min_tier: KycTier,
    per_tx_limit: Micros,
}

impl FakeKycGate {
    /// A gate requiring `min_tier` and capping each redemption at `per_tx_limit`.
    pub fn new(min_tier: KycTier, per_tx_limit_micros: i64) -> Self {
        FakeKycGate {
            min_tier,
            per_tx_limit: Micros::new(per_tx_limit_micros),
        }
    }
}

#[async_trait]
impl KycGate for FakeKycGate {
    async fn authorize(
        &self,
        _account: &AccountId,
        tier: KycTier,
        amount: Micros,
    ) -> Result<(), MintError> {
        if tier < self.min_tier {
            return Err(MintError::KycRequired);
        }
        if amount.get() > self.per_tx_limit.get() {
            return Err(MintError::PayoutLimitExceeded);
        }
        Ok(())
    }
}

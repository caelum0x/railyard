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
    /// A PayPal receiver identified by email (PayPal Payouts API).
    PayPal {
        /// The receiver's PayPal email.
        email: String,
    },
}

impl PayoutDest {
    /// The stable wire form (`{"kind":"stripe","account":..}` …) persisted on
    /// the payout record and echoed by the API.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            PayoutDest::Stripe { account } => {
                serde_json::json!({ "kind": "stripe", "account": account })
            }
            PayoutDest::Crypto { chain, address } => {
                serde_json::json!({ "kind": "crypto", "chain": chain, "address": address })
            }
            PayoutDest::PayPal { email } => serde_json::json!({ "kind": "paypal", "email": email }),
        }
    }

    /// Parse the wire form produced by [`PayoutDest::to_json`].
    pub fn from_json(v: &serde_json::Value) -> Option<PayoutDest> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        match v["kind"].as_str()? {
            "stripe" => Some(PayoutDest::Stripe {
                account: s("account")?,
            }),
            "crypto" => Some(PayoutDest::Crypto {
                chain: s("chain")?,
                address: s("address")?,
            }),
            "paypal" => Some(PayoutDest::PayPal { email: s("email")? }),
            _ => None,
        }
    }
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
    /// The provider's own id for this payout (Stripe transfer id, PayPal
    /// payout batch id, tx hash …), used to poll asynchronous settlement.
    pub provider_ref: Option<String>,
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
    /// The operation is structurally impossible for this ramp (unknown or
    /// inactive provider, a billing-only provider with no payout API, a
    /// destination kind or amount the provider cannot pay). NOT retryable.
    Unsupported,
}

/// The injectable ramp seam.
#[async_trait]
pub trait Ramp: Send + Sync {
    /// Confirm an inbound deposit (before a `mint`).
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError>;

    /// Confirm an inbound deposit in the context of the mint it would back:
    /// the Mint `account` being credited and the requested `amount`.
    ///
    /// [`crate::ReserveLedger::mint`] always calls this form. Providers whose
    /// re-query is keyed on the PAYER rather than the payment (RevenueCat
    /// `/v1/subscribers/{app_user_id}`, Adapty's profile API) need the account
    /// to find the transaction at all; they bind the returned deposit to that
    /// account via [`Deposit::bound_account`]. Every other adapter keeps the
    /// default, which ignores the context and re-queries by reference alone.
    async fn confirm_deposit_for(
        &self,
        deposit_ref: &str,
        _account: &str,
        _amount: Micros,
    ) -> Result<Deposit, RampError> {
        self.confirm_deposit(deposit_ref).await
    }

    /// Send a payout out (after a `redeem` burn). Idempotent by `payout_ref`.
    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError>;

    /// Pre-burn routability check — NO network, NO side effects. `redeem`
    /// calls this BEFORE burning so a payout that can never be delivered
    /// (unknown provider tag, billing-only provider, wrong destination kind,
    /// sub-cent fiat amount) is rejected without destroying user funds.
    /// Adapters that can pay anything they are asked to keep the default.
    async fn check_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<(), RampError> {
        Ok(())
    }

    /// Re-query the provider for the settlement state of a previously
    /// initiated payout (asynchronous providers such as PayPal Payouts).
    /// Providers whose payouts settle synchronously keep the default
    /// [`RampError::Unsupported`].
    async fn payout_status(
        &self,
        _payout_ref: &str,
        _provider_ref: Option<&str>,
    ) -> Result<PayoutStatus, RampError> {
        Err(RampError::Unsupported)
    }
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
            provider_ref: Some(format!("fake_{payout_ref}")),
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

/// The production-default [`KycGate`]: **denies every redemption**.
///
/// Mint ships with KYC *closed*. Until an operator wires a real KYC/limit
/// provider — its own impl of [`KycGate`] that can affirmatively approve a
/// redemption against verified identity — every `redeem` is refused with
/// [`MintError::KycRequired`]. Unlike the permissive [`FakeKycGate`] (which
/// trusts a caller-*presented* tier), this gate fails **closed**: a missing or
/// mis-configured provider denies the payout rather than waving it through. It
/// is the safe default for a money-moving path and is the only gate the
/// `mint-server` binary installs when running against a production database.
#[derive(Debug, Default, Clone, Copy)]
pub struct StrictKycGate;

impl StrictKycGate {
    /// A gate that denies every redemption until a real provider is wired.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl KycGate for StrictKycGate {
    async fn authorize(
        &self,
        _account: &AccountId,
        _tier: KycTier,
        _amount: Micros,
    ) -> Result<(), MintError> {
        Err(MintError::KycRequired)
    }
}

// ---- Production KYC: provider-verified identity seam ----------------------

/// The verified-identity standing of an account as reported by a KYC provider
/// (Persona / Stripe Identity). This is the account's *actual* status on file,
/// distinct from any tier a caller merely *presents* on a redeem request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KycStatus {
    /// No verification exists for the account — treat as unverified.
    Unverified,
    /// A verification exists but is not yet in a terminal approved state
    /// (pending / processing / needs-review).
    Pending,
    /// Identity verification approved at the given tier.
    Approved(KycTier),
    /// Verification was explicitly declined / failed.
    Declined,
}

/// Failures a [`KycVerifier`] provider lookup can report. Every variant is
/// treated as **deny** by [`VerifiedKycGate`] (fail closed): a KYC provider we
/// cannot reach — or is misconfigured — must never wave a money-moving payout
/// through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KycError {
    /// The provider is transiently unreachable (network / 5xx).
    ProviderUnavailable,
    /// The provider is missing required configuration (e.g. an API key).
    NotConfigured,
    /// The provider returned a response we could not interpret.
    MalformedResponse,
}

/// The injectable KYC-provider lookup seam. Mirrors the [`Ramp`] / `Signer`
/// determinism seams: the concrete Persona / Stripe Identity impls live in
/// `mint-payments` behind an `HttpTransport`; deterministic fakes (below) run in
/// tests with NO network and NO keys.
///
/// A verifier resolves an account's *provider-verified* [`KycStatus`] keyed on
/// its stable `acct_…` id (used as the provider `reference-id` / metadata key).
#[async_trait]
pub trait KycVerifier: Send + Sync {
    /// Resolve the account's current verified status from the provider.
    async fn verify(&self, account: &AccountId) -> Result<KycStatus, KycError>;
}

/// The production [`KycGate`]: authorizes a redemption ONLY against the account's
/// provider-*verified* tier (looked up live via a [`KycVerifier`]) plus a
/// per-transaction payout limit.
///
/// Unlike the dev [`FakeKycGate`], it deliberately IGNORES the caller-presented
/// tier — identity is established by the provider, never asserted by the caller —
/// and it fails **closed** on any provider error. This is the gate an operator
/// wires in production to replace the deny-by-default [`StrictKycGate`].
pub struct VerifiedKycGate<V: KycVerifier> {
    verifier: V,
    min_tier: KycTier,
    per_tx_limit: Micros,
}

impl<V: KycVerifier> VerifiedKycGate<V> {
    /// A gate that approves a redemption only when the provider reports the
    /// account verified at ≥ `min_tier` AND the amount is within `per_tx_limit`.
    pub fn new(verifier: V, min_tier: KycTier, per_tx_limit_micros: i64) -> Self {
        VerifiedKycGate {
            verifier,
            min_tier,
            per_tx_limit: Micros::new(per_tx_limit_micros),
        }
    }
}

#[async_trait]
impl<V: KycVerifier> KycGate for VerifiedKycGate<V> {
    async fn authorize(
        &self,
        account: &AccountId,
        _presented_tier: KycTier,
        amount: Micros,
    ) -> Result<(), MintError> {
        // Enforce the payout ceiling before spending a provider round-trip.
        if amount.get() > self.per_tx_limit.get() {
            return Err(MintError::PayoutLimitExceeded);
        }
        // Establish identity from the provider, NOT from the caller-presented
        // tier. Any provider failure denies (fail closed).
        let status = self
            .verifier
            .verify(account)
            .await
            .map_err(|_| MintError::KycRequired)?;
        match status {
            KycStatus::Approved(tier) if tier >= self.min_tier => Ok(()),
            _ => Err(MintError::KycRequired),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mint_core::AccountKind;
    use ulid::Ulid;

    fn acct() -> AccountId {
        AccountId::new(AccountKind::User, Ulid::new())
    }

    #[tokio::test]
    async fn strict_gate_denies_even_the_highest_tier_and_smallest_amount() {
        let gate = StrictKycGate::new();
        // Fully-verified identity, a one-microdollar redemption: still denied.
        let err = gate
            .authorize(&acct(), KycTier::Verified, Micros::new(1))
            .await
            .expect_err("strict gate must deny by default");
        assert_eq!(err, MintError::KycRequired);
    }

    #[tokio::test]
    async fn strict_gate_denies_regardless_of_amount() {
        let gate = StrictKycGate;
        for amount in [0, 1, 1_000, i64::MAX] {
            assert_eq!(
                gate.authorize(&acct(), KycTier::Verified, Micros::new(amount))
                    .await,
                Err(MintError::KycRequired),
            );
        }
    }

    #[tokio::test]
    async fn fake_gate_stays_permissive_for_contrast() {
        // The dev fake, by design, admits a caller-presented tier at/above its
        // minimum and under its limit — exactly the behavior that must NOT ship
        // in production, which is why the binary defaults to StrictKycGate.
        let gate = FakeKycGate::new(KycTier::Basic, i64::MAX);
        assert!(gate
            .authorize(&acct(), KycTier::Basic, Micros::new(999_999_999))
            .await
            .is_ok());
    }

    // ---- VerifiedKycGate: provider-verified identity + payout limit ---------

    /// A deterministic [`KycVerifier`] fake: a fixed status, or a simulated
    /// provider error. NO network.
    struct FakeKycVerifier(Result<KycStatus, KycError>);

    #[async_trait]
    impl KycVerifier for FakeKycVerifier {
        async fn verify(&self, _account: &AccountId) -> Result<KycStatus, KycError> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn verified_gate_admits_provider_approved_identity() {
        let gate = VerifiedKycGate::new(
            FakeKycVerifier(Ok(KycStatus::Approved(KycTier::Verified))),
            KycTier::Verified,
            1_000_000_000,
        );
        assert!(gate
            .authorize(&acct(), KycTier::None, Micros::new(500_000_000))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn verified_gate_ignores_a_caller_presented_tier() {
        // Caller CLAIMS Verified, but the provider says Unverified: denied.
        let gate = VerifiedKycGate::new(
            FakeKycVerifier(Ok(KycStatus::Unverified)),
            KycTier::Verified,
            1_000_000_000,
        );
        assert_eq!(
            gate.authorize(&acct(), KycTier::Verified, Micros::new(1))
                .await,
            Err(MintError::KycRequired)
        );
    }

    #[tokio::test]
    async fn verified_gate_denies_below_required_tier() {
        let gate = VerifiedKycGate::new(
            FakeKycVerifier(Ok(KycStatus::Approved(KycTier::Basic))),
            KycTier::Verified,
            1_000_000_000,
        );
        assert_eq!(
            gate.authorize(&acct(), KycTier::Verified, Micros::new(1))
                .await,
            Err(MintError::KycRequired)
        );
    }

    #[tokio::test]
    async fn verified_gate_enforces_per_tx_limit_before_network() {
        // Over the ceiling → PayoutLimitExceeded, even though the (unused here)
        // provider would have approved.
        let gate = VerifiedKycGate::new(
            FakeKycVerifier(Ok(KycStatus::Approved(KycTier::Verified))),
            KycTier::Verified,
            1_000_000,
        );
        assert_eq!(
            gate.authorize(&acct(), KycTier::Verified, Micros::new(1_000_001))
                .await,
            Err(MintError::PayoutLimitExceeded)
        );
    }

    #[tokio::test]
    async fn verified_gate_fails_closed_on_provider_error() {
        for err in [
            KycError::ProviderUnavailable,
            KycError::NotConfigured,
            KycError::MalformedResponse,
        ] {
            let gate = VerifiedKycGate::new(FakeKycVerifier(Err(err)), KycTier::Verified, i64::MAX);
            assert_eq!(
                gate.authorize(&acct(), KycTier::Verified, Micros::new(1))
                    .await,
                Err(MintError::KycRequired),
                "provider error {err:?} must deny"
            );
        }
    }
}

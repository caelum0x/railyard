//! [`ReserveLedger`] — deposit-gated mint, KYC-gated redeem, and the §6 solvency
//! proof, wrapping a store + injected [`Ramp`] + injected [`KycGate`].

use mint_core::{op_hash, Micros, MintError, TransferAmount};
use mint_receipts::{reserve_proof_from_receipts, ReserveProof};
use mint_store::{LedgerStore, MintCommit, RedeemCommit};
use serde_json::json;

use crate::ramp::{KycGate, Payout, PayoutStatus, Ramp, RampError};
use crate::{MintReceipt, MintRequest, RedeemReceipt, RedeemRequest};

/// The reserve facade: the ONLY place AIT supply changes. Every `mint`/`redeem`
/// moves the in-store reserve in lockstep with the account leg, so
/// `Σ(available + held) == reserve` holds by construction.
pub struct ReserveLedger<S: LedgerStore, R: Ramp, K: KycGate> {
    store: S,
    ramp: R,
    kyc: K,
}

impl<S: LedgerStore, R: Ramp, K: KycGate> ReserveLedger<S, R, K> {
    /// Wrap a store with an injected ramp + KYC gate.
    pub fn new(store: S, ramp: R, kyc: K) -> Self {
        ReserveLedger { store, ramp, kyc }
    }

    /// Borrow the underlying store (for balance reads / rebuild / seeding).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Borrow the injected ramp (to inspect recorded payouts in tests).
    pub fn ramp(&self) -> &R {
        &self.ramp
    }

    /// Create AIT — **deposit-gated**. AIT is minted ONLY against a confirmed
    /// ramp deposit whose amount covers the request. Idempotent per
    /// `deposit_ref`: a confirmed deposit mints EXACTLY once.
    ///
    /// - `amount_micros` must be strictly positive ([`MintError::InvalidAmount`]);
    /// - an unknown/unconfirmed deposit is [`MintError::DepositNotConfirmed`];
    /// - a deposit smaller than the request is [`MintError::DepositAmountMismatch`];
    /// - `to` must exist ([`MintError::AccountNotFound`]).
    pub async fn mint(&self, req: MintRequest) -> Result<MintReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;

        // 1. Deposit gate: the ramp must confirm a deposit that covers the mint.
        let deposit = self
            .ramp
            .confirm_deposit(&req.deposit_ref)
            .await
            .map_err(map_deposit_err)?;
        if !deposit.confirmed {
            return Err(MintError::DepositNotConfirmed);
        }
        // Bind the credit to the payment: when the provider's authenticated
        // re-query reports which account actually paid, it MUST equal the account
        // being credited. This stops a forged/unauthenticated webhook (e.g.
        // Iyzico's 3DS callback) from redirecting a real payment to an
        // attacker-chosen account.
        if let Some(bound) = deposit.bound_account.as_deref() {
            if bound != req.to.as_str() {
                return Err(MintError::DepositNotConfirmed);
            }
        }
        if deposit.amount_micros.get() < amount.get() {
            return Err(MintError::DepositAmountMismatch);
        }

        // 2. Key idempotency on the deposit_ref so a deposit mints exactly once
        //    regardless of how many times the mint is retried.
        let idem_key = format!("mint:{}", req.deposit_ref);
        // NOTE: the caller-supplied `req.idem_key` is deliberately NOT folded into
        // the op_hash. The authoritative dedupe key is `mint:{deposit_ref}`, so a
        // legitimate retry that reuses the same deposit_ref but supplies a fresh
        // per-attempt caller idem_key must replay cleanly instead of tripping
        // IdempotencyConflict. Real conflicts (a different to/amount reusing the
        // same deposit_ref) still mismatch on those fields.
        let hash = op_hash(&json!({
            "op": "mint",
            "to": req.to.as_str(),
            "amount": amount.get(),
            "deposit_ref": req.deposit_ref,
        }));

        // 3. Atomic mint: credit + reserve rise in lockstep.
        let out = self
            .store
            .commit_mint(MintCommit {
                to: req.to,
                amount,
                deposit_ref: req.deposit_ref.clone(),
                idem_key,
                op_hash: hash,
            })
            .await?;

        Ok(MintReceipt {
            first_seq: out.first_seq,
            to_balance: out.to_balance,
            reserve_after_micros: out.reserve_after_micros,
            deposit_ref: req.deposit_ref,
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// Burn AIT → fiat/crypto payout — **KYC/limit-gated**. The burn (which
    /// decrements the reserve) is atomic and happens BEFORE the payout, so a
    /// failed payout can be retried against the already-burned supply without
    /// double-burning.
    ///
    /// - `amount_micros` must be strictly positive ([`MintError::InvalidAmount`]);
    /// - below the required KYC tier is [`MintError::KycRequired`];
    /// - over the payout limit is [`MintError::PayoutLimitExceeded`];
    /// - insufficient balance is [`MintError::InsufficientFunds`];
    /// - draining the reserve below the amount is [`MintError::ReserveInsufficient`].
    ///
    /// A transient payout-provider outage does NOT fail the call: the burn is
    /// durable and the returned [`RedeemReceipt::payout`] carries
    /// [`PayoutStatus::Failed`]. Retrying `redeem` with the same `payout_ref`
    /// replays the burn (no double-burn) and re-sends the payout exactly once.
    pub async fn redeem(&self, req: RedeemRequest) -> Result<RedeemReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;

        // 1. KYC / limit gate.
        self.kyc
            .authorize(&req.from, req.kyc_tier, amount.micros())
            .await?;

        // 2. Atomic burn: available + reserve fall in lockstep.
        let idem_key = format!("redeem:{}", req.payout_ref);
        // NOTE: as in mint(), the caller-supplied `req.idem_key` is deliberately
        // NOT folded into the op_hash. The authoritative dedupe key is
        // `redeem:{payout_ref}`, so retrying with the same payout_ref but a fresh
        // caller idem_key replays the burn (no double-burn) instead of tripping
        // IdempotencyConflict. A different from/amount reusing the same payout_ref
        // still mismatches on those fields.
        let hash = op_hash(&json!({
            "op": "redeem",
            "from": req.from.as_str(),
            "amount": amount.get(),
            "payout_ref": req.payout_ref,
        }));
        let out = self
            .store
            .commit_redeem(RedeemCommit {
                from: req.from,
                amount,
                payout_ref: req.payout_ref.clone(),
                idem_key,
                op_hash: hash,
            })
            .await?;

        // 3. Post-commit payout (idempotent by payout_ref). A provider outage is
        //    surfaced as a Failed payout on the receipt, NOT a burn rollback.
        let payout = match self
            .ramp
            .initiate_payout(&req.payout_ref, &req.payout_dest, amount.micros())
            .await
        {
            Ok(p) => p,
            Err(RampError::ProviderUnavailable) => Payout {
                payout_ref: req.payout_ref.clone(),
                amount_micros: amount.micros(),
                status: PayoutStatus::Failed,
            },
            Err(_) => Payout {
                payout_ref: req.payout_ref.clone(),
                amount_micros: amount.micros(),
                status: PayoutStatus::Failed,
            },
        };

        Ok(RedeemReceipt {
            first_seq: out.first_seq,
            from_balance: out.from_balance,
            reserve_after_micros: out.reserve_after_micros,
            payout_ref: req.payout_ref,
            payout,
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// The §6 solvency gate: `supply == in-store reserve`, `backed == Σ last
    /// receipt (available + held) per account`, `ok == (backed == supply)`. With
    /// the reserve tracked in-store, `Σ(available + held) == reserve` is directly
    /// assertable.
    pub async fn solvency(&self) -> Result<ReserveProof, MintError> {
        let reserve = self.store.reserve_micros().await;
        let receipts = self.store.snapshot_receipts().await;
        Ok(reserve_proof_from_receipts(&receipts, reserve))
    }
}

/// Map a deposit-side [`RampError`] to the stable §7 mint error code.
fn map_deposit_err(e: RampError) -> MintError {
    match e {
        RampError::DepositNotFound | RampError::NotConfirmed | RampError::ProviderUnavailable => {
            MintError::DepositNotConfirmed
        }
    }
}

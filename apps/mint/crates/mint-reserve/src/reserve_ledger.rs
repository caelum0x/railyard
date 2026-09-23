//! [`ReserveLedger`] — deposit-gated mint, KYC-gated redeem, and the §6 solvency
//! proof, wrapping a store + injected [`Ramp`] + injected [`KycGate`].

use mint_core::{op_hash, Micros, MintError, TransferAmount};
use mint_receipts::{reserve_proof_from_receipts, ReserveProof};
use mint_store::{LedgerStore, MintCommit, PayoutRecord, PayoutRecordStatus, RedeemCommit};
use serde_json::json;

use crate::ramp::{KycGate, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
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
    /// - draining the reserve below the amount is [`MintError::ReserveInsufficient`];
    /// - a payout the ramp can never deliver is [`MintError::PayoutUnroutable`]
    ///   and is rejected BEFORE the burn (no funds destroyed).
    ///
    /// A transient payout-provider outage does NOT fail the call: the burn is
    /// durable, the outcome is persisted as a `Failed` payout record (listable
    /// and retryable via [`ReserveLedger::retry_payout`]) and the returned
    /// [`RedeemReceipt::payout`] carries [`PayoutStatus::Failed`]. Retrying
    /// `redeem` with the same `payout_ref` replays the burn (no double-burn) and
    /// re-sends only a payout that has not already succeeded.
    pub async fn redeem(&self, req: RedeemRequest) -> Result<RedeemReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;

        // 1. KYC / limit gate.
        self.kyc
            .authorize(&req.from, req.kyc_tier, amount.micros())
            .await?;

        // 2. Routability preflight BEFORE the burn: a payout that can never be
        //    delivered (unknown / inactive provider, billing-only provider,
        //    wrong destination kind, sub-cent fiat) is refused while the funds
        //    are still intact. Nothing is burned on this path.
        self.ramp
            .check_payout(&req.payout_ref, &req.payout_dest, amount.micros())
            .await
            .map_err(|_| MintError::PayoutUnroutable)?;

        // 3. Atomic burn: available + reserve fall in lockstep.
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
                from: req.from.clone(),
                amount,
                payout_ref: req.payout_ref.clone(),
                idem_key,
                op_hash: hash,
            })
            .await?;

        // 4. Post-commit payout, durably recorded. A replayed redeem whose payout
        //    already succeeded (or is still settling) is NOT re-sent.
        let existing = self.store.get_payout(&req.payout_ref).await?;
        let record = match existing {
            Some(rec) if rec.status != PayoutRecordStatus::Failed => rec,
            prior => {
                let base = prior.unwrap_or_else(|| PayoutRecord {
                    payout_ref: req.payout_ref.clone(),
                    account_id: req.from.clone(),
                    amount_micros: amount.micros(),
                    dest: req.payout_dest.to_json(),
                    status: PayoutRecordStatus::Pending,
                    provider_ref: None,
                    attempts: 0,
                    last_error: None,
                    burn_seq: out.first_seq,
                    created_ms: 0,
                    updated_ms: 0,
                });
                self.attempt_payout(base, &req.payout_dest).await?
            }
        };

        Ok(RedeemReceipt {
            first_seq: out.first_seq,
            from_balance: out.from_balance,
            reserve_after_micros: out.reserve_after_micros,
            payout_ref: req.payout_ref,
            payout: payout_from_record(&record),
            idempotent_replay: out.idempotent_replay,
        })
    }

    /// Re-send a failed payout against its already-burned supply. `Paid` and
    /// `Pending` payouts are returned unchanged (never double-sent); a `Failed`
    /// one is re-initiated with the SAME `payout_ref`, which every adapter uses
    /// as its provider idempotency key. `None` ⇒ no such payout.
    pub async fn retry_payout(&self, payout_ref: &str) -> Result<Option<PayoutRecord>, MintError> {
        let Some(rec) = self.store.get_payout(payout_ref).await? else {
            return Ok(None);
        };
        if rec.status != PayoutRecordStatus::Failed {
            return Ok(Some(rec));
        }
        let dest = PayoutDest::from_json(&rec.dest).ok_or(MintError::Storage)?;
        self.ramp
            .check_payout(&rec.payout_ref, &dest, rec.amount_micros)
            .await
            .map_err(|_| MintError::PayoutUnroutable)?;
        self.attempt_payout(rec, &dest).await.map(Some)
    }

    /// Poll the provider for a `Pending` payout's settlement and persist the
    /// transition. Providers without a status API leave the record unchanged.
    pub async fn refresh_payout(
        &self,
        payout_ref: &str,
    ) -> Result<Option<PayoutRecord>, MintError> {
        let Some(rec) = self.store.get_payout(payout_ref).await? else {
            return Ok(None);
        };
        if rec.status != PayoutRecordStatus::Pending {
            return Ok(Some(rec));
        }
        match self
            .ramp
            .payout_status(&rec.payout_ref, rec.provider_ref.as_deref())
            .await
        {
            Ok(status) => {
                let status = record_status(status);
                if status == rec.status {
                    return Ok(Some(rec));
                }
                let last_error = (status == PayoutRecordStatus::Failed)
                    .then(|| "provider reported the payout as failed/returned".to_string());
                self.store
                    .upsert_payout(PayoutRecord {
                        status,
                        last_error,
                        ..rec
                    })
                    .await
                    .map(Some)
            }
            // No status API / transient outage: keep the record as-is.
            Err(_) => Ok(Some(rec)),
        }
    }

    /// One `initiate_payout` attempt, persisted. Provider errors become a
    /// durable `Failed` record (with the error) rather than a lost response.
    async fn attempt_payout(
        &self,
        base: PayoutRecord,
        dest: &PayoutDest,
    ) -> Result<PayoutRecord, MintError> {
        let attempts = base.attempts.saturating_add(1);
        let outcome = self
            .ramp
            .initiate_payout(&base.payout_ref, dest, base.amount_micros)
            .await;
        let next = match outcome {
            Ok(p) => PayoutRecord {
                status: record_status(p.status),
                provider_ref: p.provider_ref.or(base.provider_ref.clone()),
                attempts,
                last_error: (p.status == PayoutStatus::Failed)
                    .then(|| "provider rejected the payout".to_string()),
                ..base
            },
            Err(e) => PayoutRecord {
                status: PayoutRecordStatus::Failed,
                attempts,
                last_error: Some(ramp_error_message(e).to_string()),
                ..base
            },
        };
        self.store.upsert_payout(next).await
    }

    /// The §6 solvency gate: `supply == in-store reserve`, `backed == Σ last
    /// receipt (available + held) per account`, `ok == (backed == supply)`. With
    /// the reserve tracked in-store, `Σ(available + held) == reserve` is directly
    /// assertable.
    ///
    /// Storage faults propagate as errors: an unreachable database must never
    /// read as `supply == backed == 0 ⇒ ok`. Only the latest receipt per account
    /// is loaded (one `DISTINCT ON` scan on Postgres).
    pub async fn solvency(&self) -> Result<ReserveProof, MintError> {
        let reserve = self.store.try_reserve_micros().await?;
        let latest = self.store.latest_receipts_per_account().await?;
        Ok(reserve_proof_from_receipts(&latest, reserve))
    }
}

/// Map a ramp payout status onto the persisted record status.
fn record_status(s: PayoutStatus) -> PayoutRecordStatus {
    match s {
        PayoutStatus::Paid => PayoutRecordStatus::Paid,
        PayoutStatus::Pending => PayoutRecordStatus::Pending,
        PayoutStatus::Failed => PayoutRecordStatus::Failed,
    }
}

/// Project a persisted record back onto the receipt-level [`Payout`].
fn payout_from_record(r: &PayoutRecord) -> Payout {
    Payout {
        payout_ref: r.payout_ref.clone(),
        amount_micros: r.amount_micros,
        status: match r.status {
            PayoutRecordStatus::Paid => PayoutStatus::Paid,
            PayoutRecordStatus::Pending => PayoutStatus::Pending,
            PayoutRecordStatus::Failed => PayoutStatus::Failed,
        },
        provider_ref: r.provider_ref.clone(),
    }
}

/// Operator-facing text for a payout-side ramp error.
fn ramp_error_message(e: RampError) -> &'static str {
    match e {
        RampError::ProviderUnavailable => "payout provider unavailable (retryable)",
        RampError::DepositNotFound => "payout provider reported the target as not found",
        RampError::NotConfirmed => "payout provider did not confirm the payout",
        RampError::Unsupported => "payout not supported by the configured provider",
    }
}

/// Map a deposit-side [`RampError`] to the stable §7 mint error code.
fn map_deposit_err(e: RampError) -> MintError {
    match e {
        RampError::DepositNotFound
        | RampError::NotConfirmed
        | RampError::ProviderUnavailable
        | RampError::Unsupported => MintError::DepositNotConfirmed,
    }
}

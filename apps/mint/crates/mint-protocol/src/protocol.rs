//! The `Aitp402` challenge / verify / settle state machine (SPEC §5.3).
//!
//! Ties together the wire types, the two encodings, the payer-key seam, and the
//! durable ledger idempotency into a deterministic, injectable (clock + keys)
//! settlement engine over an AP0-AP2 [`Ledger`]. Every settled authorization
//! flows through the existing ledger apply path, so AP2 receipts and value
//! conservation continue to hold.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier};
use mint_core::{AccountId, Address, HoldId, Micros, MintError};
use mint_ledger::{
    HoldCaptureRequest, HoldCreateRequest, HoldReceipt, HoldReleaseRequest, Ledger,
    TransferReceipt, TransferRequest,
};
use mint_receipts::Receipt;
use mint_store::{Clock, LedgerStore};
use ulid::Ulid;

use crate::intent::compute_op_hash;
use crate::types::{Authorization, Challenge, Scheme};
use crate::verifier::PayerKeys;

/// The result of a settled authorization. Carries both the ledger-level receipt
/// (balances/hold state) and the FULL §6 [`Receipt`] for the `AITP-Receipt`
/// response header (the client's lossy subset still decodes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// An `aitp-transfer` settled immediately.
    Transfer {
        /// Ledger transfer result (balances after).
        receipt: TransferReceipt,
        /// The §6 receipt for the `AITP-Receipt` header.
        aitp_receipt: Receipt,
    },
    /// An `aitp-hold` reserved funds; capture/release settles later.
    Hold {
        /// The reservation id (capture/release target).
        hold_id: HoldId,
        /// Ledger hold result (holder balance after).
        receipt: HoldReceipt,
        /// The §6 receipt for the `AITP-Receipt` header.
        aitp_receipt: Receipt,
    },
}

/// The AITP-402 server state machine, generic over the store, clock, payer-key
/// registry (all injectable for deterministic tests).
pub struct Aitp402<S: LedgerStore, C: Clock, K: PayerKeys> {
    ledger: Ledger<S>,
    clock: C,
    keys: K,
    /// Settle window added past the authorization expiry for a `hold`
    /// settlement's auto-release deadline (committed atomically with the
    /// hold). `None` ⇒ hold settlements get no deadline.
    hold_ttl_ms: Option<i64>,
}

impl<S, C, K> Aitp402<S, C, K>
where
    S: LedgerStore,
    C: Clock,
    K: PayerKeys,
{
    /// Assemble a state machine over an existing ledger.
    pub fn new(ledger: Ledger<S>, clock: C, keys: K) -> Self {
        Aitp402 {
            ledger,
            clock,
            keys,
            hold_ttl_ms: None,
        }
    }

    /// Give every `hold` settlement an auto-release deadline of
    /// `max(authorization expiry, now) + ttl_ms`, written in the same
    /// transaction as the hold so an abandoned hold can never lock the
    /// payer's funds forever.
    pub fn with_hold_ttl_ms(mut self, ttl_ms: i64) -> Self {
        self.hold_ttl_ms = Some(ttl_ms.max(1));
        self
    }

    /// Borrow the underlying ledger (seeding / balance reads / capture in tests).
    pub fn ledger(&self) -> &Ledger<S> {
        &self.ledger
    }

    /// Issue a `402` [`Challenge`] with a fresh random nonce, expiring
    /// `ttl_ms` from now. Challenge ownership stays at the protected edge;
    /// Mint settles the resulting payer-signed instruction.
    pub fn issue_challenge(
        &self,
        payee: &Address,
        amount_micros: i64,
        resource: &str,
        scheme: Scheme,
        ttl_ms: i64,
    ) -> Challenge {
        self.issue_challenge_with_nonce(
            &Ulid::new().to_string(),
            payee,
            amount_micros,
            resource,
            scheme,
            ttl_ms,
        )
    }

    /// Deterministic test seam for [`Aitp402::issue_challenge`]: mint a challenge
    /// with a caller-supplied `nonce` (no randomness).
    pub fn issue_challenge_with_nonce(
        &self,
        nonce: &str,
        payee: &Address,
        amount_micros: i64,
        resource: &str,
        scheme: Scheme,
        ttl_ms: i64,
    ) -> Challenge {
        let challenge = Challenge {
            payee: payee.as_str().to_string(),
            amount_micros: Micros::new(amount_micros),
            currency: "AIT".to_string(),
            resource: resource.to_string(),
            nonce: nonce.to_string(),
            expiry: self.clock.now() + ttl_ms,
            scheme,
        };
        challenge
    }

    /// Verify a signed [`Authorization`] and settle it (SPEC §5.3). The pipeline
    /// runs in a FIXED order so a forged/expired/underpaying authorization can
    /// never consume or DoS a legitimately-issued nonce:
    ///
    /// 1. `expiry` in the future — else [`MintError::AuthorizationExpired`];
    /// 2. recompute `op_hash`, compare to the claimed one — else
    ///    [`MintError::SignatureInvalid`] (catches post-sign field tampering);
    /// 3. Ed25519-verify `sig` vs the payer key — else `SignatureInvalid`;
    /// 4. parse payer account + payee address. The protected service is
    ///    responsible for matching the authorization to the challenge it issued;
    ///    Mint verifies the payer-signed money instruction itself;
    /// 5. settle by the signed scheme, threading a nonce-bound
    ///    `idempotency_key` (`nonce:idempotency_key`, so distinct challenges can
    ///    never share a ledger op) and `resource`.
    pub async fn verify_and_settle(&self, auth: &Authorization) -> Result<Settlement, MintError> {
        // (1) Expiry must be in the future.
        if auth.expiry <= self.clock.now() {
            return Err(MintError::AuthorizationExpired);
        }

        // (2) Recompute op_hash and compare to the claimed hex.
        let recomputed = compute_op_hash(auth);
        if recomputed.to_hex() != auth.op_hash {
            return Err(MintError::SignatureInvalid);
        }

        // (3) Ed25519-verify the signature over the 32-byte op_hash.
        let vk = self
            .keys
            .verifying_key(&auth.payer, &auth.pubkey_id)
            .ok_or(MintError::SignatureInvalid)?;
        let sig_bytes = URL_SAFE_NO_PAD
            .decode(auth.sig.as_bytes())
            .map_err(|_| MintError::SignatureInvalid)?;
        let sig = Signature::from_slice(&sig_bytes).map_err(|_| MintError::SignatureInvalid)?;
        vk.verify(recomputed.as_bytes(), &sig)
            .map_err(|_| MintError::SignatureInvalid)?;

        // (4) Parse identities. Challenge/price matching belongs to the edge
        // that issued the 402; Mint only moves funds authorized by the payer.
        let payer = AccountId::parse(&auth.payer)?;
        let payee_addr = Address::parse(&auth.payee)?;
        let scheme = auth.scheme;

        // (5) Settle by scheme through the existing ledger apply path.
        //
        // Bind the ledger idempotency key to the SINGLE-USE nonce (which is not
        // in the ledger's money-moving op_hash) so two DISTINCT challenges can
        // never collapse to one ledger op. Without this, a payer could reuse one
        // `idempotency_key` across fresh nonces with an identical
        // payer/payee/amount/resource body and be charged once while every
        // request is served (pay-once-use-many). The ledger's durable
        // idempotency index is the authoritative replay store. An exact replay
        // returns the original settlement (marked `idempotent_replay`) so a
        // caller that lost the HTTP response can recover the durable hold id or
        // receipt after restart. It never moves money twice. Serving a request
        // twice is prevented at the edge by the request execution lease.
        let amount = auth.amount_micros.get();
        let idem_key = format!("{}:{}", auth.nonce, auth.idempotency_key);
        match scheme {
            Scheme::Transfer => {
                let payee = self.ledger.store().lookup_address(&payee_addr).await?;
                let receipt = self
                    .ledger
                    .transfer(TransferRequest {
                        from: payer,
                        to: payee,
                        amount_micros: amount,
                        idem_key: idem_key.clone(),
                        ref_: Some(auth.resource.clone()),
                    })
                    .await?;
                let aitp_receipt = self
                    .receipt_for(receipt.first_seq, &receipt.from_balance.account_id)
                    .await?;
                Ok(Settlement::Transfer {
                    receipt,
                    aitp_receipt,
                })
            }
            Scheme::Hold => {
                let receipt = self
                    .ledger
                    .create_hold(HoldCreateRequest {
                        from: payer,
                        amount_micros: amount,
                        idem_key: idem_key.clone(),
                        ref_: Some(auth.resource.clone()),
                        expires_at_ms: self
                            .hold_ttl_ms
                            .map(|ttl| auth.expiry.max(self.clock.now()).saturating_add(ttl)),
                    })
                    .await?;
                let hold_id = receipt.hold_id.clone();
                let aitp_receipt = self
                    .receipt_for(
                        receipt.holder_balance.updated_seq,
                        &receipt.holder_balance.account_id,
                    )
                    .await?;
                Ok(Settlement::Hold {
                    hold_id,
                    receipt,
                    aitp_receipt,
                })
            }
        }
    }

    /// Capture an `aitp-hold` for the actual usage (the success path). Passes
    /// through to [`Ledger::capture_hold`]: settles `actual_micros` to `to` and
    /// releases the remainder to the payer.
    pub async fn capture(
        &self,
        hold_id: HoldId,
        actual_micros: i64,
        to: AccountId,
        idem_key: impl Into<String>,
    ) -> Result<HoldReceipt, MintError> {
        self.ledger
            .capture_hold(HoldCaptureRequest {
                hold_id,
                amount_micros: actual_micros,
                to,
                idem_key: idem_key.into(),
                ref_: None,
            })
            .await
    }

    /// Release an `aitp-hold` with NO settlement (the failure path — the agent
    /// pays nothing, ADR-0002). Passes through to [`Ledger::release_hold`].
    pub async fn release(
        &self,
        hold_id: HoldId,
        idem_key: impl Into<String>,
    ) -> Result<HoldReceipt, MintError> {
        self.ledger
            .release_hold(HoldReleaseRequest {
                hold_id,
                idem_key: idem_key.into(),
                ref_: None,
            })
            .await
    }

    /// Resolve the exact §6 receipt emitted by an operation. Looking up the
    /// operation's journal sequence and principal is safe under concurrent
    /// settlements; reading the global "last receipt" is not.
    async fn receipt_for(
        &self,
        journal_seq: u64,
        account_id: &AccountId,
    ) -> Result<Receipt, MintError> {
        // Point lookup of this settlement's principal receipt (never a scan of
        // the whole receipt log). A committed settlement always appended it; a
        // miss is a storage fault, surfaced as an error instead of a panic.
        self.ledger
            .receipt(account_id, journal_seq)
            .await
            .ok_or(MintError::Storage)
    }
}

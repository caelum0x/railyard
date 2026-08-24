//! The `Aitp402` challenge / verify / settle state machine (SPEC §5.3).
//!
//! Ties together the wire types, the two encodings, the payer-key seam, and the
//! nonce store into a deterministic, injectable (clock + signer + keys + nonces)
//! settlement engine over an AP0-AP2 [`Ledger`]. Every settled authorization
//! flows through the existing ledger apply path, so AP2 receipts and value
//! conservation continue to hold.

use std::collections::HashMap;
use std::sync::Mutex;

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
use crate::nonce::NonceStore;
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
/// registry, and nonce store (all injectable for deterministic tests).
pub struct Aitp402<S: LedgerStore, C: Clock, K: PayerKeys, N: NonceStore> {
    ledger: Ledger<S>,
    clock: C,
    keys: K,
    nonces: N,
    /// Challenges issued by this server, keyed by nonce. `verify_and_settle`
    /// looks up the challenge to recover its `scheme` and to reject an
    /// authorization that does not match the price/payee/resource it issued (so
    /// a payer can neither underpay nor redirect a signed intent).
    issued: Mutex<HashMap<String, Challenge>>,
}

impl<S, C, K, N> Aitp402<S, C, K, N>
where
    S: LedgerStore,
    C: Clock,
    K: PayerKeys,
    N: NonceStore,
{
    /// Assemble a state machine over an existing ledger.
    pub fn new(ledger: Ledger<S>, clock: C, keys: K, nonces: N) -> Self {
        Aitp402 {
            ledger,
            clock,
            keys,
            nonces,
            issued: Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the underlying ledger (seeding / balance reads / capture in tests).
    pub fn ledger(&self) -> &Ledger<S> {
        &self.ledger
    }

    /// Issue a `402` [`Challenge`] with a fresh random nonce, expiring
    /// `ttl_ms` from now. Registers the challenge so a later authorization for
    /// this nonce can be settled.
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
        self.issued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(nonce.to_string(), challenge.clone());
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
    /// 4. parse payer account + payee address, and match the auth against the
    ///    issued challenge (no underpay/redirect) — else `bad_account_id` /
    ///    `bad_address` / `signature_invalid`;
    /// 5. claim the nonce (LAST, so a bad auth never burns it) — else
    ///    [`MintError::NonceReplayed`];
    /// 6. settle by the challenge scheme, threading a nonce-bound
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

        // (4) Parse identities and match the auth against the issued challenge.
        let payer = AccountId::parse(&auth.payer)?;
        let payee_addr = Address::parse(&auth.payee)?;
        let scheme = {
            let issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            let challenge = issued.get(&auth.nonce).ok_or(MintError::SignatureInvalid)?;
            if challenge.payee != auth.payee
                || challenge.amount_micros != auth.amount_micros
                || challenge.resource != auth.resource
                || challenge.expiry != auth.expiry
            {
                return Err(MintError::SignatureInvalid);
            }
            challenge.scheme
        };

        // (5) Claim the nonce — only now that the auth is proven valid.
        self.nonces.claim(&auth.nonce, auth.expiry)?;

        // (6) Settle by scheme through the existing ledger apply path.
        //
        // Bind the ledger idempotency key to the SINGLE-USE nonce (which is not
        // in the ledger's money-moving op_hash) so two DISTINCT challenges can
        // never collapse to one ledger op. Without this, a payer could reuse one
        // `idempotency_key` across fresh nonces with an identical
        // payer/payee/amount/resource body and be charged once while every
        // request is served (pay-once-use-many).
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
                let aitp_receipt = self.last_receipt().await;
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
                    })
                    .await?;
                let hold_id = receipt.hold_id.clone();
                let aitp_receipt = self.last_receipt().await;
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

    /// The §6 receipt a just-committed settlement appended (its last row). A
    /// settlement always runs on a freshly-claimed nonce, so it is never an
    /// idempotent no-op here and always appends at least one receipt.
    async fn last_receipt(&self) -> Receipt {
        self.ledger
            .receipts()
            .await
            .last()
            .cloned()
            .expect("a settlement always appends a receipt")
    }
}

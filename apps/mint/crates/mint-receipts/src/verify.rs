//! Offline verification: recompute every receipt's hash, check the per-account
//! `prev_hash` chain, check `account_seq` contiguity, and check the Ed25519
//! signature against a public-key-only [`Keyring`]. No store and no private key
//! are needed, so a client can verify its full history offline.

use std::collections::HashMap;

use ed25519_dalek::{Signature, Verifier};
use mint_core::AccountId;

use crate::receipt::Receipt;
use crate::signer::Keyring;

/// Why a receipt log failed verification. Every tamper (edited amount/balance/
/// ts/op_hash/from/to, reordered receipts, forged signature, broken link) maps
/// to one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// A receipt's `prev_hash` does not equal the prior receipt's `hash` (or the
    /// genesis receipt's `prev_hash` is not `ZERO`).
    ChainBroken {
        /// The account whose chain broke.
        account: AccountId,
        /// The `account_seq` at which the link failed.
        account_seq: u64,
    },
    /// A receipt's stored `hash` does not equal `body.compute_hash()` — the body
    /// was mutated after signing.
    HashMismatch {
        /// The account whose receipt was mutated.
        account: AccountId,
        /// The `account_seq` of the mutated receipt.
        account_seq: u64,
    },
    /// The Ed25519 signature does not verify against the keyring key.
    BadSignature {
        /// The account whose receipt signature failed.
        account: AccountId,
        /// The `account_seq` of the receipt.
        account_seq: u64,
    },
    /// `account_seq` is not the contiguous `0,1,2,…` expected for the chain
    /// (a receipt was dropped, duplicated, or reordered).
    NonContiguousSeq {
        /// The account whose sequence is non-contiguous.
        account: AccountId,
        /// The position in the chain where the expectation broke.
        expected: u64,
        /// The `account_seq` actually found.
        found: u64,
    },
    /// The receipt's `signer` id is absent from the keyring.
    UnknownSigner {
        /// The account whose receipt referenced an unknown signer.
        account: AccountId,
        /// The `account_seq` of the receipt.
        account_seq: u64,
    },
}

/// Verify a single account's chain (already filtered + in append order):
/// genesis `prev_hash == ZERO`, each `prev_hash == prev.hash`, recomputed body
/// hash == stored hash, `account_seq` contiguous from 0, and a valid signature.
pub fn verify_chain(chain: &[Receipt], keyring: &Keyring) -> Result<(), VerifyError> {
    use crate::receipt::ReceiptHash;

    for (idx, receipt) in chain.iter().enumerate() {
        let account = receipt.body.account_id.clone();
        let account_seq = receipt.body.account_seq;

        // 1. account_seq must be contiguous 0..n.
        let expected = idx as u64;
        if account_seq != expected {
            return Err(VerifyError::NonContiguousSeq {
                account,
                expected,
                found: account_seq,
            });
        }

        // 2. prev_hash chain: genesis is ZERO, else the prior receipt's hash.
        let expected_prev = if idx == 0 {
            ReceiptHash::ZERO
        } else {
            chain[idx - 1].hash
        };
        if receipt.body.prev_hash != expected_prev {
            return Err(VerifyError::ChainBroken {
                account,
                account_seq,
            });
        }

        // 3. Recompute the body hash; any body mutation is caught here.
        if receipt.body.compute_hash() != receipt.hash {
            return Err(VerifyError::HashMismatch {
                account,
                account_seq,
            });
        }

        // 4. Signature verifies against the registered public key.
        let vk = keyring
            .get(&receipt.body.signer)
            .ok_or_else(|| VerifyError::UnknownSigner {
                account: account.clone(),
                account_seq,
            })?;
        let sig = Signature::from_bytes(receipt.sig.as_bytes());
        if vk.verify(receipt.hash.as_bytes(), &sig).is_err() {
            return Err(VerifyError::BadSignature {
                account,
                account_seq,
            });
        }
    }
    Ok(())
}

/// Verify the whole log: group receipts by account **preserving global append
/// order within each group**, then run [`verify_chain`] per account.
pub fn verify_log(all: &[Receipt], keyring: &Keyring) -> Result<(), VerifyError> {
    // Preserve first-seen account order for stable, deterministic errors.
    let mut order: Vec<AccountId> = Vec::new();
    let mut groups: HashMap<AccountId, Vec<Receipt>> = HashMap::new();
    for r in all {
        let acct = r.body.account_id.clone();
        if !groups.contains_key(&acct) {
            order.push(acct.clone());
        }
        groups.entry(acct).or_default().push(r.clone());
    }
    for acct in order {
        verify_chain(&groups[&acct], keyring)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{ReceiptDraft, ReceiptLog};
    use crate::receipt::{ReceiptHash, ReceiptOp};
    use crate::signer::FixedSigner;
    use mint_core::{AccountId, AccountKind, Micros};
    use ulid::Ulid;

    fn acct() -> AccountId {
        AccountId::new(AccountKind::User, Ulid::new())
    }

    fn draft(account: &AccountId, journal_seq: u64, amt: i64) -> ReceiptDraft {
        ReceiptDraft {
            account_id: account.clone(),
            journal_seq,
            op: ReceiptOp::Transfer,
            op_hash: mint_core::op_hash(&serde_json::json!({ "seq": journal_seq })),
            amount_micros: Micros::new(amt),
            from: None,
            to: Some(account.clone()),
            balance_after_available_micros: Micros::new(amt),
            balance_after_held_micros: Micros::zero(),
            ts: 1_700_000_000_000,
        }
    }

    fn build_log() -> (ReceiptLog, FixedSigner, AccountId, AccountId) {
        let signer = FixedSigner::new();
        let mut log = ReceiptLog::new();
        let a = acct();
        let b = acct();
        log.append(draft(&a, 0, 100), &signer);
        log.append(draft(&b, 1, 200), &signer);
        log.append(draft(&a, 2, 300), &signer);
        log.append(draft(&b, 3, 400), &signer);
        (log, signer, a, b)
    }

    #[test]
    fn valid_log_verifies() {
        let (log, signer, _a, _b) = build_log();
        let kr = Keyring::with_signer(&signer);
        assert!(verify_log(log.global(), &kr).is_ok());
    }

    #[test]
    fn tampered_amount_is_hash_mismatch() {
        let (log, signer, _a, _b) = build_log();
        let kr = Keyring::with_signer(&signer);
        let mut all = log.global().to_vec();
        all[0].body.amount_micros = Micros::new(999);
        assert!(matches!(
            verify_log(&all, &kr),
            Err(VerifyError::HashMismatch { .. })
        ));
    }

    #[test]
    fn flipped_sig_byte_is_bad_signature() {
        let (log, signer, _a, _b) = build_log();
        let kr = Keyring::with_signer(&signer);
        let mut all = log.global().to_vec();
        let mut sig_bytes = *all[0].sig.as_bytes();
        sig_bytes[0] ^= 0x01;
        all[0].sig = crate::receipt::ReceiptSig::from_bytes(sig_bytes);
        assert!(matches!(
            verify_log(&all, &kr),
            Err(VerifyError::BadSignature { .. })
        ));
    }

    #[test]
    fn broken_prev_hash_is_chain_broken() {
        let (log, signer, _a, _b) = build_log();
        let kr = Keyring::with_signer(&signer);
        let mut all = log.global().to_vec();
        // Mutating prev_hash also changes the recomputed hash, but the chain
        // check runs first for a non-genesis receipt whose prev no longer links.
        // Use the genesis receipt: set its prev_hash to non-zero.
        all[0].body.prev_hash = ReceiptHash::from_bytes([9u8; 32]);
        assert!(matches!(
            verify_log(&all, &kr),
            Err(VerifyError::ChainBroken { .. })
        ));
    }

    #[test]
    fn unknown_signer_is_reported() {
        let (log, _signer, _a, _b) = build_log();
        let empty = Keyring::new();
        assert!(matches!(
            verify_log(log.global(), &empty),
            Err(VerifyError::UnknownSigner { .. })
        ));
    }

    #[test]
    fn reordered_same_account_receipts_fail() {
        let (log, signer, a, _b) = build_log();
        let kr = Keyring::with_signer(&signer);
        // Swap account a's two receipts in the global vector.
        let mut all = log.global().to_vec();
        let a_positions: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, r)| r.body.account_id == a)
            .map(|(i, _)| i)
            .collect();
        all.swap(a_positions[0], a_positions[1]);
        // After the swap, account a's grouped chain is out of account_seq order.
        assert!(verify_log(&all, &kr).is_err());
    }
}

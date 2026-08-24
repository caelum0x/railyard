//! Proof-of-reserves helper (§6 `/reserve`).
//!
//! Sums every account's backing (`available + held`) and compares it to the
//! claimed circulating `supply`. `ok == (backed == supply)`. The
//! `_from_receipts` variant folds the LAST receipt per account (its
//! `balance_after_*` fields are the account's current state).

use std::collections::HashMap;

use mint_core::{AccountId, Micros};

use crate::receipt::Receipt;

/// The result of a reserve check: circulating `supply`, total `backed` (Σ of
/// every account's available + held), and whether they match exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReserveProof {
    /// The claimed circulating supply (µ$).
    pub supply_micros: Micros,
    /// The total backing found across all accounts (µ$).
    pub backed_micros: Micros,
    /// `true` iff `backed_micros == supply_micros`.
    pub ok: bool,
}

/// Fold `(available, held)` pairs into a [`ReserveProof`] against `supply`.
/// Saturates on overflow so a pathological input can never panic the auditor
/// (the resulting mismatch still reports `ok == false`).
pub fn reserve_proof(
    balances: impl Iterator<Item = (Micros, Micros)>,
    supply: Micros,
) -> ReserveProof {
    let mut backed: i64 = 0;
    for (avail, held) in balances {
        backed = backed
            .saturating_add(avail.get())
            .saturating_add(held.get());
    }
    let backed = Micros::new(backed);
    ReserveProof {
        supply_micros: supply,
        backed_micros: backed,
        ok: backed == supply,
    }
}

/// Fold the LAST receipt per account (in global append order) into a reserve
/// proof: each account's latest `balance_after_available + balance_after_held`
/// is its current backing.
pub fn reserve_proof_from_receipts(all: &[Receipt], supply: Micros) -> ReserveProof {
    // Keep the most-recent receipt per account (append order ⇒ later wins).
    let mut last: HashMap<AccountId, (Micros, Micros)> = HashMap::new();
    for r in all {
        last.insert(
            r.body.account_id.clone(),
            (
                r.body.balance_after_available_micros,
                r.body.balance_after_held_micros,
            ),
        );
    }
    reserve_proof(last.into_values(), supply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{ReceiptDraft, ReceiptLog};
    use crate::receipt::ReceiptOp;
    use crate::signer::FixedSigner;
    use mint_core::{AccountId, AccountKind};
    use ulid::Ulid;

    fn acct() -> AccountId {
        AccountId::new(AccountKind::User, Ulid::new())
    }

    #[test]
    fn balances_matching_supply_are_ok() {
        let proof = reserve_proof(
            vec![
                (Micros::new(600_000), Micros::new(0)),
                (Micros::new(300_000), Micros::new(100_000)),
            ]
            .into_iter(),
            Micros::new(1_000_000),
        );
        assert!(proof.ok);
        assert_eq!(proof.backed_micros, Micros::new(1_000_000));
    }

    #[test]
    fn shortfall_is_not_ok() {
        let proof = reserve_proof(
            vec![(Micros::new(500_000), Micros::new(0))].into_iter(),
            Micros::new(1_000_000),
        );
        assert!(!proof.ok);
    }

    #[test]
    fn from_receipts_uses_latest_per_account() {
        let signer = FixedSigner::new();
        let mut log = ReceiptLog::new();
        let a = acct();

        // Two receipts on `a`: only the latest balance (700k avail + 300k held)
        // should count toward the reserve.
        let mk = |seq: u64, avail: i64, held: i64| ReceiptDraft {
            account_id: a.clone(),
            journal_seq: seq,
            op: ReceiptOp::Transfer,
            op_hash: mint_core::op_hash(&serde_json::json!({ "seq": seq })),
            amount_micros: Micros::new(1),
            from: None,
            to: Some(a.clone()),
            balance_after_available_micros: Micros::new(avail),
            balance_after_held_micros: Micros::new(held),
            ts: 1_700_000_000_000,
        };
        log.append(mk(0, 1_000_000, 0), &signer);
        log.append(mk(1, 700_000, 300_000), &signer);

        let proof = reserve_proof_from_receipts(log.global(), Micros::new(1_000_000));
        assert!(proof.ok);
        assert_eq!(proof.backed_micros, Micros::new(1_000_000));

        // A corrupted supply claim fails.
        let bad = reserve_proof_from_receipts(log.global(), Micros::new(2_000_000));
        assert!(!bad.ok);
    }
}

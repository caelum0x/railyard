//! The append-only receipt log.
//!
//! Callers hand a [`ReceiptDraft`] (everything they already know about a settled
//! leg); [`ReceiptLog::append`] derives the chain-local fields — `account_seq`
//! and `prev_hash` from the account's current head — builds the [`ReceiptBody`],
//! computes its `hash`, signs it, and pushes it onto the global append-ordered
//! vector. The per-account head advances lock-step, so the chain can never
//! diverge from the emission order.

use std::collections::HashMap;

use mint_core::{AccountId, Micros, OpHash};

use crate::receipt::{Receipt, ReceiptBody, ReceiptHash, ReceiptOp};
use crate::signer::Signer;

/// Everything the caller knows about one settled leg. The log fills in
/// `account_seq`, `prev_hash`, `hash`, and `sig`.
#[derive(Clone, Debug)]
pub struct ReceiptDraft {
    /// Whose chain this receipt joins.
    pub account_id: AccountId,
    /// FK to the emitting journal row's `seq`.
    pub journal_seq: u64,
    /// The op verb.
    pub op: ReceiptOp,
    /// The shared op hash of the emitting op.
    pub op_hash: OpHash,
    /// The moved/settled amount for this leg.
    pub amount_micros: Micros,
    /// Debited counterparty (`None` for seed).
    pub from: Option<AccountId>,
    /// Credited counterparty (`None` for release).
    pub to: Option<AccountId>,
    /// Post-op available balance for this account.
    pub balance_after_available_micros: Micros,
    /// Post-op held balance for this account.
    pub balance_after_held_micros: Micros,
    /// The op's timestamp (unix millis) — SAME value stamped on its journal rows.
    pub ts: i64,
}

/// A per-account hash-linked, signed receipt log with a global append-order view.
#[derive(Default, Clone)]
pub struct ReceiptLog {
    receipts: Vec<Receipt>,
    /// Per-account head: `(last_account_seq, last_hash)`.
    heads: HashMap<AccountId, (u64, ReceiptHash)>,
}

impl ReceiptLog {
    /// A fresh empty log.
    pub fn new() -> Self {
        ReceiptLog {
            receipts: Vec::new(),
            heads: HashMap::new(),
        }
    }

    /// Append a receipt derived from `draft`, signed by `signer`. Returns a
    /// reference to the newly-appended receipt.
    ///
    /// - `account_seq` = previous head seq + 1, or `0` for the account's first;
    /// - `prev_hash`   = previous head hash, or [`ReceiptHash::ZERO`] at genesis;
    /// - `hash`        = `body.compute_hash()`;
    /// - `sig`         = `signer.sign(hash)`.
    pub fn append(&mut self, draft: ReceiptDraft, signer: &dyn Signer) -> &Receipt {
        let (account_seq, prev_hash) = match self.heads.get(&draft.account_id) {
            Some((seq, hash)) => (seq + 1, *hash),
            None => (0, ReceiptHash::ZERO),
        };

        let body = ReceiptBody {
            account_id: draft.account_id.clone(),
            account_seq,
            journal_seq: draft.journal_seq,
            op: draft.op,
            op_hash: draft.op_hash,
            amount_micros: draft.amount_micros,
            from: draft.from,
            to: draft.to,
            balance_after_available_micros: draft.balance_after_available_micros,
            balance_after_held_micros: draft.balance_after_held_micros,
            ts: draft.ts,
            prev_hash,
            signer: signer.key_id(),
        };

        let hash = body.compute_hash();
        let sig = signer.sign(hash.as_bytes());

        self.heads
            .insert(draft.account_id.clone(), (account_seq, hash));
        self.receipts.push(Receipt { body, hash, sig });
        self.receipts.last().expect("just pushed a receipt")
    }

    /// The full log in global append order.
    pub fn global(&self) -> &[Receipt] {
        &self.receipts
    }

    /// This account's chain, in append order (a filtered view of the global log).
    pub fn chain(&self, account: &AccountId) -> Vec<&Receipt> {
        self.receipts
            .iter()
            .filter(|r| &r.body.account_id == account)
            .collect()
    }

    /// The current head hash of an account's chain (`None` if it has no receipts).
    pub fn head(&self, account: &AccountId) -> Option<ReceiptHash> {
        self.heads.get(account).map(|(_, h)| *h)
    }

    /// Number of receipts in the log.
    pub fn len(&self) -> usize {
        self.receipts.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::FixedSigner;
    use mint_core::AccountKind;
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

    #[test]
    fn chain_links_prev_hash_and_increments_account_seq() {
        let signer = FixedSigner::new();
        let mut log = ReceiptLog::new();
        let a = acct();

        log.append(draft(&a, 0, 100), &signer);
        log.append(draft(&a, 1, 200), &signer);
        log.append(draft(&a, 2, 300), &signer);

        let chain = log.chain(&a);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].body.prev_hash, ReceiptHash::ZERO);
        assert_eq!(chain[0].body.account_seq, 0);
        assert_eq!(chain[1].body.prev_hash, chain[0].hash);
        assert_eq!(chain[1].body.account_seq, 1);
        assert_eq!(chain[2].body.prev_hash, chain[1].hash);
        assert_eq!(chain[2].body.account_seq, 2);
        assert_eq!(log.head(&a), Some(chain[2].hash));
    }

    #[test]
    fn independent_accounts_have_independent_chains() {
        let signer = FixedSigner::new();
        let mut log = ReceiptLog::new();
        let a = acct();
        let b = acct();

        log.append(draft(&a, 0, 100), &signer);
        log.append(draft(&b, 1, 100), &signer);
        log.append(draft(&a, 2, 100), &signer);

        // Both chains start at genesis.
        assert_eq!(log.chain(&a)[0].body.prev_hash, ReceiptHash::ZERO);
        assert_eq!(log.chain(&b)[0].body.prev_hash, ReceiptHash::ZERO);
        assert_eq!(log.chain(&a).len(), 2);
        assert_eq!(log.chain(&b).len(), 1);
        // Global order preserved.
        assert_eq!(log.global().len(), 3);
    }
}

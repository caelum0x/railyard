//! A binary BLAKE3 Merkle tree over the global receipt log (§6 audit root).
//!
//! Leaves are the receipts' `hash` values in global append order. Domain
//! separation prevents leaf/node confusion: `leaf = blake3(0x00 || hash)`,
//! `node = blake3(0x01 || left || right)`. An odd level duplicates its last
//! node. An empty log has root [`ReceiptHash::ZERO`].

use crate::receipt::{Receipt, ReceiptHash};

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// Hash one leaf: `blake3(0x00 || receipt_hash)`.
fn leaf_hash(h: &ReceiptHash) -> ReceiptHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[LEAF_PREFIX]);
    hasher.update(h.as_bytes());
    ReceiptHash::from_bytes(*hasher.finalize().as_bytes())
}

/// Hash an internal node: `blake3(0x01 || left || right)`.
fn node_hash(l: &ReceiptHash, r: &ReceiptHash) -> ReceiptHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[NODE_PREFIX]);
    hasher.update(l.as_bytes());
    hasher.update(r.as_bytes());
    ReceiptHash::from_bytes(*hasher.finalize().as_bytes())
}

/// Build the Merkle levels bottom-up. `levels[0]` is the leaf layer; the last
/// level holds the single root. `None` for an empty leaf set.
fn build_levels(all: &[Receipt]) -> Option<Vec<Vec<ReceiptHash>>> {
    if all.is_empty() {
        return None;
    }
    let leaves: Vec<ReceiptHash> = all.iter().map(|r| leaf_hash(&r.hash)).collect();
    let mut levels = vec![leaves];
    while levels.last().expect("non-empty").len() > 1 {
        let cur = levels.last().expect("non-empty");
        let mut next = Vec::with_capacity(cur.len().div_ceil(2));
        let mut i = 0;
        while i < cur.len() {
            let left = cur[i];
            // Duplicate the last node on an odd level.
            let right = if i + 1 < cur.len() {
                cur[i + 1]
            } else {
                cur[i]
            };
            next.push(node_hash(&left, &right));
            i += 2;
        }
        levels.push(next);
    }
    Some(levels)
}

/// The Merkle root over the global log. Empty log ⇒ [`ReceiptHash::ZERO`].
pub fn merkle_root(all: &[Receipt]) -> ReceiptHash {
    match build_levels(all) {
        None => ReceiptHash::ZERO,
        Some(levels) => levels.last().expect("at least one level")[0],
    }
}

/// `true` iff `merkle_root(all) == claimed`.
pub fn verify_merkle(all: &[Receipt], claimed: ReceiptHash) -> bool {
    merkle_root(all) == claimed
}

/// An inclusion proof for one leaf: the sibling hashes from leaf to root, each
/// tagged with whether the sibling sits on the right of the current node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MerkleProof {
    /// The leaf's index in the global log.
    pub index: usize,
    /// `(sibling_hash, sibling_is_right)` for each level, leaf → root.
    pub siblings: Vec<(ReceiptHash, bool)>,
}

impl MerkleProof {
    /// Recompute the root from a `receipt.hash` leaf and this proof, and check it
    /// equals `root`.
    pub fn verify(&self, leaf: ReceiptHash, root: ReceiptHash) -> bool {
        let mut acc = leaf_hash(&leaf);
        for (sib, sib_is_right) in &self.siblings {
            acc = if *sib_is_right {
                node_hash(&acc, sib)
            } else {
                node_hash(sib, &acc)
            };
        }
        acc == root
    }
}

/// Produce an inclusion proof for the receipt at `index` in `all`. Returns
/// `None` if `index` is out of range or the log is empty.
pub fn prove(all: &[Receipt], index: usize) -> Option<MerkleProof> {
    let levels = build_levels(all)?;
    if index >= all.len() {
        return None;
    }
    let mut siblings = Vec::new();
    let mut idx = index;
    // Walk every level except the root; record the sibling at each.
    for level in &levels[..levels.len() - 1] {
        let sibling_idx = if idx.is_multiple_of(2) {
            // Right sibling; duplicate self on an odd level.
            if idx + 1 < level.len() {
                idx + 1
            } else {
                idx
            }
        } else {
            idx - 1
        };
        let sib_is_right = sibling_idx >= idx;
        siblings.push((level[sibling_idx], sib_is_right));
        idx /= 2;
    }
    Some(MerkleProof { index, siblings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{ReceiptDraft, ReceiptLog};
    use crate::receipt::ReceiptOp;
    use crate::signer::FixedSigner;
    use mint_core::{AccountId, AccountKind, Micros};
    use ulid::Ulid;

    fn build(n: usize) -> ReceiptLog {
        let signer = FixedSigner::new();
        let mut log = ReceiptLog::new();
        let a = AccountId::new(AccountKind::User, Ulid::new());
        for i in 0..n {
            log.append(
                ReceiptDraft {
                    account_id: a.clone(),
                    journal_seq: i as u64,
                    op: ReceiptOp::Transfer,
                    op_hash: mint_core::op_hash(&serde_json::json!({ "i": i })),
                    amount_micros: Micros::new(100 + i as i64),
                    from: None,
                    to: Some(a.clone()),
                    balance_after_available_micros: Micros::new(100),
                    balance_after_held_micros: Micros::zero(),
                    ts: 1_700_000_000_000,
                },
                &signer,
            );
        }
        log
    }

    #[test]
    fn empty_log_root_is_zero() {
        assert_eq!(merkle_root(&[]), ReceiptHash::ZERO);
    }

    #[test]
    fn root_verifies_and_editing_a_leaf_changes_it() {
        let log = build(5); // odd count exercises last-leaf duplication
        let all = log.global();
        let root = merkle_root(all);
        assert!(verify_merkle(all, root));

        let mut edited = all.to_vec();
        edited[2].hash = ReceiptHash::from_bytes([0xAB; 32]);
        assert_ne!(merkle_root(&edited), root);
        assert!(!verify_merkle(&edited, root));
    }

    #[test]
    fn inclusion_proof_holds_for_every_index() {
        let log = build(6);
        let all = log.global();
        let root = merkle_root(all);
        for i in 0..all.len() {
            let proof = prove(all, i).unwrap();
            assert!(
                proof.verify(all[i].hash, root),
                "proof for index {i} must verify"
            );
        }
        assert!(prove(all, all.len()).is_none());
    }

    #[test]
    fn single_leaf_root_is_leaf_hash() {
        let log = build(1);
        let all = log.global();
        assert_eq!(merkle_root(all), leaf_hash(&all[0].hash));
    }
}

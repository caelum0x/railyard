//! The rebuildable balance projection.
//!
//! Balances are a cache derived from the journal; [`crate::LedgerStore::rebuild_balances`]
//! folds the journal from `seq 0` and must equal the live projection bit-for-bit
//! (the AP0 acceptance gate, extended in AP1 to include the held leg).
//! `held_micros` is live and rebuildable in AP1: `rebuild_balances` routes each
//! journal delta to the leg named on its `bucket`, reconstructing held
//! bit-exact.

use mint_core::{AccountId, Micros};
use serde::{Deserialize, Serialize};

/// A single account's derived balance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    /// Which account this balance is for.
    pub account_id: AccountId,
    /// Spendable microdollars.
    pub available_micros: Micros,
    /// Reserved by open holds/streams. Live and rebuildable as of AP1.
    pub held_micros: Micros,
    /// The `seq` of the last journal row applied to this account.
    pub updated_seq: u64,
}

impl Balance {
    /// A fresh zeroed balance for a newly created account.
    pub fn zeroed(account_id: AccountId) -> Self {
        Balance {
            account_id,
            available_micros: Micros::zero(),
            held_micros: Micros::zero(),
            updated_seq: 0,
        }
    }
}

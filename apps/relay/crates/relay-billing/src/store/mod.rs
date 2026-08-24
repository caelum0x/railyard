//! Async trait-based storage seam — the ONE abstraction TensorZero re-implements.
//!
//! Abstracts API keys, the append-only microdollar ledger, reservations, and
//! usage records behind the [`Store`] trait, with a pure [`InMemoryStore`] impl
//! so tests have ZERO external deps. A later phase swaps `InMemoryStore` for a
//! `PgStore`/ClickHouse-backed impl behind the same trait — the critical method
//! `try_reserve` MUST stay atomic (SELECT ... FOR UPDATE or equivalent).

pub mod error;
pub mod records;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::domain::{AccountId, JobId};
use crate::money::Micros;

pub use error::StoreError;
pub use records::{now_ms, LedgerEntry, LedgerKind, StoredApiKey, Timestamp, UsageRecord};

/// API-key persistence.
#[async_trait]
pub trait KeyStore: Send + Sync {
    /// Look up an active-or-inactive key by its sha256 hash. `None` if unknown.
    async fn find_key_by_hash(&self, key_hash: &str) -> Result<Option<StoredApiKey>, StoreError>;

    /// Insert (or seed) a key row.
    async fn put_key(&self, key: StoredApiKey) -> Result<(), StoreError>;
}

/// Outcome of an atomic [`LedgerStore::try_reserve`] compare-and-append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveOutcome {
    /// The hold entry was appended; the account covered the estimate.
    Reserved,
    /// The hold was refused: the summed balance was short of the estimate.
    Insufficient { available: Micros },
}

/// Append-only microdollar ledger persistence.
#[async_trait]
pub trait LedgerStore: Send + Sync {
    /// Append a batch of entries atomically. Immutable-append: prior entries are
    /// never mutated. A batch either fully lands or (on error) does not.
    async fn append_ledger(&self, entries: Vec<LedgerEntry>) -> Result<(), StoreError>;

    /// Atomically enforce the credit gate and place a hold under ONE lock/txn:
    /// compute the account's summed balance and, only if it covers `estimate`,
    /// append `hold_entry`. This closes the check-then-append TOCTOU race — two
    /// concurrent reserves for the same account can never both pass the gate and
    /// overspend. Returns [`ReserveOutcome::Insufficient`] (appending nothing)
    /// when the balance is short.
    async fn try_reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        hold_entry: LedgerEntry,
    ) -> Result<ReserveOutcome, StoreError>;

    /// All ledger entries for an account, in append order.
    async fn ledger_for_account(
        &self,
        account_id: &AccountId,
    ) -> Result<Vec<LedgerEntry>, StoreError>;

    /// Exact integer balance = sum of every `delta_micros` for the account.
    async fn balance_micros(&self, account_id: &AccountId) -> Result<Micros, StoreError>;
}

/// Usage-record persistence (the completed-job counterpart of each Debit).
#[async_trait]
pub trait UsageStore: Send + Sync {
    /// Append a usage record.
    async fn put_usage(&self, record: UsageRecord) -> Result<(), StoreError>;

    /// All usage records for a job (normally exactly one on success).
    async fn usage_for_job(&self, job_id: &JobId) -> Result<Vec<UsageRecord>, StoreError>;
}

/// The bundled storage seam the gateway injects. Any type implementing the three
/// sub-traits is automatically a `Store`.
pub trait Store: KeyStore + LedgerStore + UsageStore + Send + Sync {}
impl<T: KeyStore + LedgerStore + UsageStore + Send + Sync> Store for T {}

/// Internal, mutex-guarded state. Never exposed; all access goes through the
/// trait methods so the append-only invariant is centralized.
#[derive(Default)]
struct InMemoryState {
    keys: Vec<StoredApiKey>,
    ledger: Vec<LedgerEntry>,
    usage: Vec<UsageRecord>,
}

/// A pure in-memory [`Store`]. `Clone` shares one backing state (`Arc`), so a
/// clone handed to the ledger service and another to the key service observe
/// the same ledger. Zero external dependencies.
#[derive(Clone, Default)]
pub struct InMemoryStore {
    state: Arc<Mutex<InMemoryState>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, InMemoryState>, StoreError> {
        self.state.lock().map_err(|_| StoreError::Lock)
    }
}

#[async_trait]
impl KeyStore for InMemoryStore {
    async fn find_key_by_hash(&self, key_hash: &str) -> Result<Option<StoredApiKey>, StoreError> {
        let state = self.lock()?;
        Ok(state.keys.iter().find(|k| k.key_hash == key_hash).cloned())
    }

    async fn put_key(&self, key: StoredApiKey) -> Result<(), StoreError> {
        let mut state = self.lock()?;
        state.keys.push(key);
        Ok(())
    }
}

#[async_trait]
impl LedgerStore for InMemoryStore {
    async fn append_ledger(&self, entries: Vec<LedgerEntry>) -> Result<(), StoreError> {
        let mut state = self.lock()?;
        // Immutable-append: extend, never touch prior entries. The whole batch
        // lands under one lock so a settle's Release+Debit are atomic.
        state.ledger.extend(entries);
        Ok(())
    }

    async fn try_reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        hold_entry: LedgerEntry,
    ) -> Result<ReserveOutcome, StoreError> {
        // One lock guards the whole compare-and-append: no other reserve/settle
        // can interleave between the balance read and the hold push.
        let mut state = self.lock()?;
        let mut total: i64 = 0;
        for e in state.ledger.iter().filter(|e| &e.account_id == account_id) {
            total =
                total
                    .checked_add(e.delta_micros.get())
                    .ok_or_else(|| StoreError::Overflow {
                        account: account_id.to_string(),
                    })?;
        }
        let available = Micros::new(total);
        if available < estimate {
            return Ok(ReserveOutcome::Insufficient { available });
        }
        state.ledger.push(hold_entry);
        Ok(ReserveOutcome::Reserved)
    }

    async fn ledger_for_account(
        &self,
        account_id: &AccountId,
    ) -> Result<Vec<LedgerEntry>, StoreError> {
        let state = self.lock()?;
        Ok(state
            .ledger
            .iter()
            .filter(|e| &e.account_id == account_id)
            .cloned()
            .collect())
    }

    async fn balance_micros(&self, account_id: &AccountId) -> Result<Micros, StoreError> {
        let state = self.lock()?;
        let mut total: i64 = 0;
        for e in state.ledger.iter().filter(|e| &e.account_id == account_id) {
            total =
                total
                    .checked_add(e.delta_micros.get())
                    .ok_or_else(|| StoreError::Overflow {
                        account: account_id.to_string(),
                    })?;
        }
        Ok(Micros::new(total))
    }
}

#[async_trait]
impl UsageStore for InMemoryStore {
    async fn put_usage(&self, record: UsageRecord) -> Result<(), StoreError> {
        let mut state = self.lock()?;
        state.usage.push(record);
        Ok(())
    }

    async fn usage_for_job(&self, job_id: &JobId) -> Result<Vec<UsageRecord>, StoreError> {
        let state = self.lock()?;
        Ok(state
            .usage
            .iter()
            .filter(|r| &r.job_id == job_id)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ModelId;
    use crate::usage::Usage;

    fn acct() -> AccountId {
        AccountId::new("acct_test")
    }

    fn deposit(account: &AccountId, micros: i64) -> LedgerEntry {
        LedgerEntry::new(
            account.clone(),
            LedgerKind::Deposit,
            Micros::new(micros),
            "seed",
            None,
        )
    }

    #[tokio::test]
    async fn key_roundtrip_by_hash() {
        let store = InMemoryStore::new();
        let key = StoredApiKey::active("hash_abc", acct());
        store.put_key(key.clone()).await.unwrap();

        let found = store.find_key_by_hash("hash_abc").await.unwrap();
        assert_eq!(found, Some(key));
        assert!(store.find_key_by_hash("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ledger_append_is_additive_and_balance_is_exact_sum() {
        let store = InMemoryStore::new();
        let a = acct();

        store
            .append_ledger(vec![deposit(&a, 20_000_000)])
            .await
            .unwrap();
        assert_eq!(
            store.balance_micros(&a).await.unwrap(),
            Micros::new(20_000_000)
        );

        let job = JobId::new();
        store
            .append_ledger(vec![LedgerEntry::new(
                a.clone(),
                LedgerKind::Reserve,
                Micros::new(-5_000_000),
                "hold",
                Some(job),
            )])
            .await
            .unwrap();
        assert_eq!(
            store.balance_micros(&a).await.unwrap(),
            Micros::new(15_000_000)
        );

        let entries = store.ledger_for_account(&a).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, LedgerKind::Deposit);
        assert_eq!(entries[1].kind, LedgerKind::Reserve);
    }

    #[tokio::test]
    async fn balance_overflow_is_reported_not_wrapped() {
        let store = InMemoryStore::new();
        let a = acct();
        store
            .append_ledger(vec![deposit(&a, i64::MAX)])
            .await
            .unwrap();
        store.append_ledger(vec![deposit(&a, 1)]).await.unwrap();
        let err = store.balance_micros(&a).await.unwrap_err();
        assert!(matches!(err, StoreError::Overflow { .. }));
    }

    #[tokio::test]
    async fn usage_roundtrip_by_job() {
        let store = InMemoryStore::new();
        let a = acct();
        let job = JobId::new();
        let rec = UsageRecord::new(
            job,
            a.clone(),
            ModelId::namespace("openai/gpt-4o"),
            Usage::new(10, 20, 30, Micros::new(1500)),
        );
        store.put_usage(rec.clone()).await.unwrap();

        let found = store.usage_for_job(&job).await.unwrap();
        assert_eq!(found, vec![rec]);
        assert!(store.usage_for_job(&JobId::new()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_is_object_safe_behind_arc_dyn() {
        let dynamic: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let a = acct();
        dynamic.append_ledger(vec![deposit(&a, 42)]).await.unwrap();
        assert_eq!(dynamic.balance_micros(&a).await.unwrap(), Micros::new(42));
    }
}

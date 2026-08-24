//! Single-use nonce store for replay protection (SPEC §5.4).
//!
//! A challenge nonce is usable **exactly once within its validity window**.
//! [`NonceStore::claim`] inserts `nonce -> expiry_ms`; re-presenting a live nonce
//! is [`MintError::NonceReplayed`]. The store is clock-driven and deterministic:
//! it drops entries whose expiry has passed (per the injected [`Clock`]) so
//! memory is bounded by *live* nonces, and there is NO wall-clock and NO
//! randomness on this path.
//!
//! Nonce vs idempotency are DISTINCT, layered guards. The nonce guards the
//! AUTHORIZATION (one settlement per challenge; reuse ⇒ `NonceReplayed`); the
//! `idempotency_key` guards the LEDGER op (a genuine retry replays the original
//! receipt as a no-op). The protocol treats nonce reuse as an ERROR — idempotent
//! no-op replay is achieved through the ledger `idem_key` path, not by reusing a
//! nonce.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mint_core::MintError;
use mint_store::Clock;

/// The replay-protection seam. `claim` is the single-use gate; `sweep` lets a
/// caller proactively evict expired entries.
pub trait NonceStore: Send + Sync {
    /// Claim `nonce` (valid until `expiry_ms`). Returns
    /// [`MintError::NonceReplayed`] if the nonce is already present and still
    /// live; otherwise records it and returns `Ok`.
    fn claim(&self, nonce: &str, expiry_ms: i64) -> Result<(), MintError>;

    /// Drop every recorded nonce whose expiry is at/before `now_ms`.
    fn sweep(&self, now_ms: i64);
}

/// An in-memory [`NonceStore`]: a `Mutex<HashMap<nonce, expiry_ms>>` driven by an
/// injected [`Clock`] (a `FixedClock` in tests). Bounded by live nonces.
pub struct InMemoryNonceStore {
    seen: Mutex<HashMap<String, i64>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryNonceStore {
    /// Build a nonce store driven by `clock` (share the SAME `Arc` as the
    /// protocol/store so time advances everywhere at once in tests).
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        InMemoryNonceStore {
            seen: Mutex::new(HashMap::new()),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl NonceStore for InMemoryNonceStore {
    fn claim(&self, nonce: &str, expiry_ms: i64) -> Result<(), MintError> {
        let now = self.clock.now();
        let mut seen = self.lock();
        // Evict expired entries first so memory stays bounded by live nonces and
        // a stale (expired) nonce can be re-issued cleanly.
        seen.retain(|_, exp| *exp > now);
        if seen.contains_key(nonce) {
            // Present after eviction ⇒ still live ⇒ a genuine replay.
            return Err(MintError::NonceReplayed);
        }
        seen.insert(nonce.to_string(), expiry_ms);
        Ok(())
    }

    fn sweep(&self, now_ms: i64) {
        self.lock().retain(|_, exp| *exp > now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mint_store::FixedClock;

    fn store(now: i64) -> (Arc<FixedClock>, InMemoryNonceStore) {
        let clock = Arc::new(FixedClock::new(now));
        let store = InMemoryNonceStore::new(clock.clone());
        (clock, store)
    }

    #[test]
    fn first_claim_ok_second_is_replay() {
        let (_c, s) = store(1_000);
        assert!(s.claim("n1", 2_000).is_ok());
        assert_eq!(s.claim("n1", 2_000).unwrap_err(), MintError::NonceReplayed);
    }

    #[test]
    fn expired_nonce_can_be_reclaimed() {
        let (clock, s) = store(1_000);
        assert!(s.claim("n1", 1_500).is_ok());
        // Advance past expiry: the stale entry is evicted and the nonce is free.
        clock.set(2_000);
        assert!(s.claim("n1", 3_000).is_ok());
    }

    #[test]
    fn distinct_nonces_coexist() {
        let (_c, s) = store(0);
        assert!(s.claim("a", 10).is_ok());
        assert!(s.claim("b", 10).is_ok());
    }

    #[test]
    fn sweep_evicts_expired() {
        let (_c, s) = store(0);
        s.claim("a", 5).unwrap();
        s.claim("b", 50).unwrap();
        s.sweep(10);
        // `a` was swept (expiry 5 <= 10) so it can be reclaimed; `b` still lives.
        assert!(s.claim("a", 100).is_ok());
        assert_eq!(s.claim("b", 50).unwrap_err(), MintError::NonceReplayed);
    }
}

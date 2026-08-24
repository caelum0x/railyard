//! Single-use nonce replay guard at the Turnstile edge (SPEC §5.1).
//!
//! Every issued challenge carries a fresh nonce; a valid authorization must echo
//! it, and Turnstile [`consume`](NonceStore::consume)s it exactly once. A second
//! request bearing the same nonce is rejected as a replay even if the signature
//! and mint would otherwise accept it. Expired nonces are swept eagerly on
//! consume and can be bulk-cleared via [`sweep`](NonceStore::sweep).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::TurnstileError;

/// A store of live, single-use nonces keyed to their expiry (unix ms).
///
/// Implementors must be safe to share across request handlers. The default
/// [`InMemoryNonceStore`] fits a single Turnstile process; a Redis-backed store
/// dropping in behind this trait handles a horizontally-scaled fleet.
pub trait NonceStore: Send + Sync {
    /// Record a freshly-issued nonce and the unix-ms time it expires.
    fn register(&self, nonce: &str, expiry_ms: i64);

    /// Consume a nonce exactly once. `Ok(())` only if it was registered, unused,
    /// and not yet expired; otherwise a replay/expiry [`TurnstileError`].
    fn consume(&self, nonce: &str, now_ms: i64) -> Result<(), TurnstileError>;

    /// Drop every nonce whose expiry is `< now_ms`.
    fn sweep(&self, now_ms: i64);
}

/// Process-local [`NonceStore`] backed by a mutex-guarded map. Suitable for a
/// single Turnstile instance and for tests.
#[derive(Default)]
pub struct InMemoryNonceStore {
    /// nonce → expiry (unix ms).
    inner: Mutex<HashMap<String, i64>>,
}

impl InMemoryNonceStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of currently-tracked (unconsumed, unswept) nonces.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl NonceStore for InMemoryNonceStore {
    fn register(&self, nonce: &str, expiry_ms: i64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(nonce.to_string(), expiry_ms);
    }

    fn consume(&self, nonce: &str, now_ms: i64) -> Result<(), TurnstileError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.remove(nonce) {
            None => Err(TurnstileError::NonceReplayed(nonce.to_string())),
            Some(expiry) if expiry < now_ms => Err(TurnstileError::NonceExpired),
            Some(_) => Ok(()),
        }
    }

    fn sweep(&self, now_ms: i64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, expiry| *expiry >= now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumes_a_live_nonce_exactly_once() {
        let store = InMemoryNonceStore::new();
        store.register("abc", 1_000);
        assert!(store.consume("abc", 500).is_ok());
        // Second use is a replay.
        assert!(matches!(
            store.consume("abc", 500),
            Err(TurnstileError::NonceReplayed(_))
        ));
    }

    #[test]
    fn rejects_expired_nonce_and_sweeps() {
        let store = InMemoryNonceStore::new();
        store.register("old", 100);
        assert!(matches!(
            store.consume("old", 200),
            Err(TurnstileError::NonceExpired)
        ));
        store.register("gone", 100);
        store.sweep(200);
        assert!(store.is_empty());
    }
}

//! Single-use nonce replay guard at the Turnstile edge (SPEC §5.1).
//!
//! Every issued challenge carries a fresh nonce; a valid authorization must echo
//! it, and Turnstile [`consume`](NonceStore::consume)s it exactly once. A second
//! request bearing the same nonce is rejected as a replay even if the signature
//! and mint would otherwise accept it. Expired nonces are swept eagerly on
//! consume and can be bulk-cleared via [`sweep`](NonceStore::sweep).
//!
//! The store is a persistence port so a single Turnstile process (the default
//! [`InMemoryNonceStore`]) and a horizontally-scaled fleet (the durable Postgres
//! [`PostgresNonceStore`](crate::pg::PostgresNonceStore), behind the `postgres`
//! feature) both drop in behind the same async trait. The trait is async because
//! a shared, durable backend does network I/O on every operation.

use std::collections::HashMap;
use std::sync::Mutex;

use aitp_client::PaymentChallenge;
use async_trait::async_trait;

use crate::error::TurnstileError;

/// A store of live, single-use challenges keyed by nonce.
///
/// Implementors must be safe to share across request handlers. The default
/// [`InMemoryNonceStore`] fits a single Turnstile process; the durable
/// [`PostgresNonceStore`](crate::pg::PostgresNonceStore) fits a horizontally
/// scaled fleet sharing one transactional store.
#[async_trait]
pub trait NonceStore: Send + Sync {
    /// Record the exact challenge that was sent to the caller.
    ///
    /// Re-registering an identical challenge is idempotent. Reusing a nonce for
    /// different challenge content is a fail-closed ownership conflict.
    async fn register(&self, challenge: &PaymentChallenge) -> Result<(), TurnstileError>;

    /// Fetch the exact issued challenge without consuming it.
    async fn get(&self, nonce: &str) -> Result<Option<PaymentChallenge>, TurnstileError>;

    /// Consume a nonce exactly once. `Ok(())` only if it was registered, unused,
    /// and not yet expired; otherwise a replay/expiry [`TurnstileError`].
    ///
    /// The consume must be atomic: under concurrent requests bearing the same
    /// nonce, at most one may observe `Ok(())`.
    async fn consume(&self, nonce: &str, now_ms: i64) -> Result<PaymentChallenge, TurnstileError>;

    /// Drop every nonce whose expiry is `< now_ms`. Returns the number removed so
    /// a reaper can report/observe its progress.
    async fn sweep(&self, now_ms: i64) -> Result<u64, TurnstileError>;
}

/// Process-local [`NonceStore`] backed by a mutex-guarded map. Suitable for a
/// single Turnstile instance and for tests.
#[derive(Default)]
pub struct InMemoryNonceStore {
    /// nonce → exact issued challenge.
    inner: Mutex<HashMap<String, PaymentChallenge>>,
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

#[async_trait]
impl NonceStore for InMemoryNonceStore {
    async fn register(&self, challenge: &PaymentChallenge) -> Result<(), TurnstileError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(&challenge.nonce) {
            Some(existing) if existing == challenge => return Ok(()),
            Some(_) => {
                return Err(TurnstileError::ChallengeMismatch(
                    "nonce is already owned by another challenge".to_string(),
                ))
            }
            None => {}
        }
        guard.insert(challenge.nonce.clone(), challenge.clone());
        Ok(())
    }

    async fn get(&self, nonce: &str) -> Result<Option<PaymentChallenge>, TurnstileError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(nonce)
            .cloned())
    }

    async fn consume(&self, nonce: &str, now_ms: i64) -> Result<PaymentChallenge, TurnstileError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.remove(nonce) {
            None => Err(TurnstileError::NonceReplayed(nonce.to_string())),
            Some(challenge) if challenge.expiry < now_ms => Err(TurnstileError::NonceExpired),
            Some(challenge) => Ok(challenge),
        }
    }

    async fn sweep(&self, now_ms: i64) -> Result<u64, TurnstileError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = guard.len();
        guard.retain(|_, challenge| challenge.expiry >= now_ms);
        Ok((before - guard.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(nonce: &str, expiry: i64) -> PaymentChallenge {
        PaymentChallenge {
            payee: "ait1qpayee".into(),
            amount_micros: aitp_client::AmountMicros(1_000),
            currency: "AIT".into(),
            resource: "POST /v1/echo".into(),
            nonce: nonce.into(),
            expiry,
            scheme: aitp_client::Scheme::Hold,
        }
    }

    #[tokio::test]
    async fn consumes_a_live_nonce_exactly_once() {
        let store = InMemoryNonceStore::new();
        let issued = challenge("abc", 1_000);
        store.register(&issued).await.unwrap();
        assert_eq!(store.get("abc").await.unwrap(), Some(issued));
        assert!(store.consume("abc", 500).await.is_ok());
        // Second use is a replay.
        assert!(matches!(
            store.consume("abc", 500).await,
            Err(TurnstileError::NonceReplayed(_))
        ));
    }

    #[tokio::test]
    async fn rejects_expired_nonce_and_sweeps() {
        let store = InMemoryNonceStore::new();
        store.register(&challenge("old", 100)).await.unwrap();
        assert!(matches!(
            store.consume("old", 200).await,
            Err(TurnstileError::NonceExpired)
        ));
        store.register(&challenge("gone", 100)).await.unwrap();
        assert_eq!(store.sweep(200).await.unwrap(), 1);
        assert!(store.is_empty());
    }
}

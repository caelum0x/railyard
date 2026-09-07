//! Durable, transactional [`NonceStore`] on Postgres — the horizontally-scaled
//! fleet backend (behind the `postgres` feature).
//!
//! The in-memory [`InMemoryNonceStore`](crate::nonce::InMemoryNonceStore) keeps
//! nonces in one process, so two Turnstile replicas cannot see each other's
//! challenges: a nonce issued by replica A is unknown to replica B and every
//! cross-replica authorization would be rejected as a replay. This store moves
//! the nonce set into a shared Postgres table so any replica can register,
//! consume-exactly-once, and sweep the same nonces.
//!
//! Mirrors `vend-store`'s conventions: the sqlx RUNTIME query API only
//! (`sqlx::query` / `query_as` + `.bind`, never the compile-time `query!`
//! macros) so the crate builds with NO `DATABASE_URL`; the schema ships as an
//! embedded migration applied at runtime via [`PostgresNonceStore::migrate`].
//!
//! Single-use is enforced atomically by `DELETE ... RETURNING`: concurrent
//! consumers racing on the same nonce contend on the row, and Postgres hands the
//! deleted row to exactly one of them — the rest see no row and are rejected as
//! replays.

use std::sync::Arc;
use std::time::Duration;

use aitp_client::PaymentChallenge;
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::clock::now_ms;
use crate::error::TurnstileError;
use crate::nonce::NonceStore;

fn store_err(e: sqlx::Error) -> TurnstileError {
    TurnstileError::Storage(e.to_string())
}

fn decode_challenge(encoded: &str) -> Result<PaymentChallenge, TurnstileError> {
    serde_json::from_str(encoded)
        .map_err(|error| TurnstileError::Storage(format!("invalid stored challenge: {error}")))
}

/// A Postgres-backed [`NonceStore`]. Cheap to clone (wraps a connection pool).
#[derive(Clone)]
pub struct PostgresNonceStore {
    pool: PgPool,
}

impl PostgresNonceStore {
    /// Connect to Postgres and build a bounded pool. Does NOT run migrations.
    pub async fn connect(database_url: &str) -> Result<Self, TurnstileError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(store_err)?;
        Ok(Self { pool })
    }

    /// Wrap an existing pool (used by tests that manage their own pool).
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply the embedded migration. Idempotent — safe to call on every startup.
    pub async fn migrate(&self) -> Result<(), TurnstileError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|e| TurnstileError::Storage(e.to_string()))
    }

    /// Reaper: drop every nonce that has already expired as of now. Returns the
    /// number removed. Expired nonces are also rejected on [`consume`](NonceStore::consume)
    /// (fail-closed), so this is purely bounded-storage housekeeping.
    pub async fn reap(&self) -> Result<u64, TurnstileError> {
        self.sweep(now_ms()).await
    }
}

#[async_trait]
impl NonceStore for PostgresNonceStore {
    async fn register(&self, challenge: &PaymentChallenge) -> Result<(), TurnstileError> {
        let encoded = serde_json::to_string(challenge)
            .map_err(|error| TurnstileError::Storage(error.to_string()))?;
        sqlx::query(
            "INSERT INTO turnstile_nonces (nonce, expiry_ms, challenge_json) \
             VALUES ($1, $2, $3) ON CONFLICT (nonce) DO NOTHING",
        )
        .bind(&challenge.nonce)
        .bind(challenge.expiry)
        .bind(&encoded)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        match self.get(&challenge.nonce).await? {
            Some(existing) if existing == *challenge => Ok(()),
            Some(_) => Err(TurnstileError::ChallengeMismatch(
                "nonce is already owned by another challenge".to_string(),
            )),
            None => Err(TurnstileError::Storage(
                "challenge disappeared while registering".to_string(),
            )),
        }
    }

    async fn get(&self, nonce: &str) -> Result<Option<PaymentChallenge>, TurnstileError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT challenge_json FROM turnstile_nonces WHERE nonce = $1")
                .bind(nonce)
                .fetch_optional(&self.pool)
                .await
                .map_err(store_err)?;
        match row {
            None => Ok(None),
            Some((Some(encoded),)) => decode_challenge(&encoded).map(Some),
            Some((None,)) => Err(TurnstileError::Storage(
                "nonce has no persisted challenge ownership".to_string(),
            )),
        }
    }

    async fn consume(&self, nonce: &str, now_ms: i64) -> Result<PaymentChallenge, TurnstileError> {
        // Atomic single-use: the row is claimed by whoever wins the DELETE.
        // A second consumer of the same nonce finds no row → replay.
        let row: Option<(i64, Option<String>)> = sqlx::query_as(
            "DELETE FROM turnstile_nonces WHERE nonce = $1 \
             RETURNING expiry_ms, challenge_json",
        )
        .bind(nonce)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;
        match row {
            None => Err(TurnstileError::NonceReplayed(nonce.to_string())),
            Some((expiry, _)) if expiry < now_ms => Err(TurnstileError::NonceExpired),
            Some((_, Some(encoded))) => decode_challenge(&encoded),
            Some((_, None)) => Err(TurnstileError::Storage(
                "nonce has no persisted challenge ownership".to_string(),
            )),
        }
    }

    async fn sweep(&self, now_ms: i64) -> Result<u64, TurnstileError> {
        let result = sqlx::query("DELETE FROM turnstile_nonces WHERE expiry_ms < $1")
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected())
    }
}

/// Spawn a background reaper that periodically drops expired nonces from the
/// shared store, bounding table growth for a long-running fleet. Returns the task
/// handle; drop it to detach or `.abort()` it on shutdown. A failed sweep is
/// logged to stderr and retried on the next tick (expired nonces still fail
/// closed on consume in the meantime).
#[must_use]
pub fn spawn_reaper(
    store: Arc<PostgresNonceStore>,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        // Skip the immediate first tick; wait a full interval before the first sweep.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = store.reap().await {
                eprintln!("turnstile nonce reaper sweep failed: {e}");
            }
        }
    })
}

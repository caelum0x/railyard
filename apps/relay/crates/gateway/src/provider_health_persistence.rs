//! Durable warm-start checkpoints for Relay's adaptive provider ranker.
//!
//! The live scorer remains lock-local and fast. Postgres stores periodic
//! snapshots so a gateway restart does not erase learned latency, error, and
//! circuit-breaker state. Billing and request correctness never depend on this
//! best-effort optimization path.

use relay_storage_postgres::PostgresProviderHealthStore;
use sqlx::PgPool;

/// Restore all valid provider records. Malformed individual rows are skipped so
/// one operator-edited snapshot cannot prevent the gateway from starting.
pub async fn restore(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let snapshots = PostgresProviderHealthStore::new(pool.clone())
        .load()
        .await?;
    Ok(tensorzero_core::model::restore_provider_health(snapshots))
}

/// Atomically upsert one consistent snapshot of every observed provider.
pub async fn persist(pool: &PgPool) -> Result<usize, String> {
    let snapshots = tensorzero_core::model::provider_health_snapshot();
    PostgresProviderHealthStore::new(pool.clone())
        .persist(&snapshots)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use relay_hedge::{
        CircuitState, InMemoryProviderHealth, Outcome, ProviderHealth, ProviderHealthSnapshot,
    };

    #[test]
    fn persisted_json_round_trip_retains_adaptive_state() {
        let original = InMemoryProviderHealth::with_defaults();
        for _ in 0..5 {
            original.record("model", "provider", Outcome::transient_error());
        }
        let encoded = serde_json::to_value(original.snapshot().remove(0)).unwrap();
        let decoded: ProviderHealthSnapshot = serde_json::from_value(encoded).unwrap();

        let restored = InMemoryProviderHealth::with_defaults();
        assert_eq!(restored.restore(vec![decoded]), 1);
        assert!(matches!(
            restored.circuit_state("model", "provider"),
            Some(CircuitState::Open { .. })
        ));
    }
}

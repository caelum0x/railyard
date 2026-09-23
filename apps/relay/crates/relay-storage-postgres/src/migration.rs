//! Relay-only migration entry point.
//!
//! TensorZero's full migration graph includes observability, optimization, and
//! configuration tables. Relay applications that only need the durable payment
//! lifecycle can apply this ordered subset without depending on those systems.

use sqlx::PgPool;

// Stable, Relay-owned Postgres advisory-lock namespace. The transaction-scoped
// lock serializes schema setup when several gateway replicas start together.
const RELAY_MIGRATION_LOCK_ID: i64 = 0x5245_4c41_594d_4947;
const CREATE_MIGRATION_LEDGER: &str = "
CREATE TABLE IF NOT EXISTS relay_schema_migrations (
    name TEXT PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)";

/// One immutable Relay schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayMigration {
    pub name: &'static str,
    pub sql: &'static str,
}

macro_rules! migration {
    ($name:literal) => {
        RelayMigration {
            name: $name,
            sql: include_str!(concat!(
                "../../tensorzero-core/src/db/postgres/migrations/",
                $name,
                ".sql"
            )),
        }
    };
}

/// Ordered schema required by Relay billing, AITP, recovery, and cost delivery.
pub const RELAY_MIGRATIONS: &[RelayMigration] = &[
    migration!("20260827170000_relay_settlement_evidence"),
    migration!("20260828120000_relay_billing_outbox"),
    migration!("20260828180000_relay_batch_billing"),
    migration!("20260828190000_relay_provider_health"),
    migration!("20260828200000_relay_cost_events"),
    migration!("20260828201000_relay_aitp_challenges"),
    migration!("20260923120000_relay_key_budgets"),
    migration!("20260924120000_relay_metered_batches"),
    migration!("20260924121000_relay_catalog_price_overrides"),
    migration!("20260924122000_relay_provider_credentials"),
];

/// Apply the Relay schema subset in chronological order.
///
/// Every migration is idempotent. The function deliberately returns the first
/// Postgres error so startup fails closed instead of serving paid traffic over
/// an incomplete durability schema.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(RELAY_MIGRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(CREATE_MIGRATION_LEDGER)
        .execute(&mut *transaction)
        .await?;
    for migration in RELAY_MIGRATIONS {
        let already_applied = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM relay_schema_migrations WHERE name=$1)",
        )
        .bind(migration.name)
        .fetch_one(&mut *transaction)
        .await?;
        if already_applied {
            continue;
        }
        sqlx::raw_sql(migration.sql)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO relay_schema_migrations (name) VALUES ($1)")
            .bind(migration.name)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use googletest::prelude::*;

    use super::*;

    #[gtest]
    fn manifest_is_unique_ordered_and_nonempty() {
        let mut previous = "";
        let mut seen = HashSet::new();
        for migration in RELAY_MIGRATIONS {
            expect_that!(migration.name, gt(previous));
            expect_that!(migration.sql.trim(), not(eq("")));
            expect_that!(seen.insert(migration.name), eq(true));
            previous = migration.name;
        }
        expect_that!(RELAY_MIGRATIONS.len(), eq(10));
    }

    #[gtest]
    fn manifest_owns_every_durable_relay_table() {
        let mut joined = RELAY_MIGRATIONS
            .iter()
            .map(|migration| migration.sql)
            .collect::<Vec<_>>()
            .join("\n");
        joined.push_str(CREATE_MIGRATION_LEDGER);
        for table in [
            "relay_schema_migrations",
            "relay_settlement_evidence",
            "relay_billing_outbox",
            "relay_provider_health",
            "relay_cost_events",
            "relay_aitp_challenges",
            "relay_key_budgets",
            "relay_metered_batches",
            "relay_catalog_price_overrides",
            "relay_provider_credentials",
        ] {
            expect_that!(joined.as_str(), contains_substring(table));
        }
    }
}

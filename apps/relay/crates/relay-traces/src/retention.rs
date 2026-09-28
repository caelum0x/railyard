//! Trace retention (owner decision OD-RY2).
//!
//! Default: full prompt and output bodies are kept 14 days, metadata (the
//! trace list rows: model, tokens, latency, cost, key) 90 days. The job deletes
//! only rows whose `created_at` is older than the configured window, in
//! batches, against a cutoff fixed once at the start of the run. Tables that do
//! not exist are skipped. `dry_run` reports what would be removed and deletes
//! nothing.
//!
//! This is separate from TensorZero's partition-drop retention
//! (`tensorzero.retention_config`), which stays untouched; row-level deletes
//! keep the windows exact rather than month-granular.

use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};

use crate::valid_schema_name;

/// OD-RY2 default: 14 days of full bodies.
pub const DEFAULT_BODY_DAYS: i64 = 14;
/// OD-RY2 default: 90 days of metadata.
pub const DEFAULT_METADATA_DAYS: i64 = 90;
/// Rows removed per `DELETE` statement.
pub const DEFAULT_BATCH_SIZE: i64 = 5_000;
/// Upper bound accepted for either window (10 years).
pub const MAX_RETENTION_DAYS: i64 = 3_650;

pub const BODY_DAYS_ENV: &str = "RELAY_TRACE_BODY_RETENTION_DAYS";
pub const METADATA_DAYS_ENV: &str = "RELAY_TRACE_METADATA_RETENTION_DAYS";

/// Tables holding prompt/output bodies (daily partitions).
pub const BODY_TABLES: [&str; 3] = [
    "chat_inference_data",
    "json_inference_data",
    "model_inference_data",
];
/// Tables holding trace metadata (monthly partitions).
pub const METADATA_TABLES: [&str; 3] = ["chat_inferences", "json_inferences", "model_inferences"];

#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    #[error("{name} must be an integer between 1 and {MAX_RETENTION_DAYS}, got `{value}`")]
    InvalidDays { name: &'static str, value: String },
    #[error("body retention ({body} days) must not exceed metadata retention ({metadata} days)")]
    BodyLongerThanMetadata { body: i64, metadata: i64 },
    #[error("batch size must be at least 1")]
    InvalidBatch,
    #[error("invalid schema name `{0}`")]
    InvalidSchema(String),
    #[error("retention query failed: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub body_days: i64,
    pub metadata_days: i64,
    pub batch_size: i64,
    pub dry_run: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            body_days: DEFAULT_BODY_DAYS,
            metadata_days: DEFAULT_METADATA_DAYS,
            batch_size: DEFAULT_BATCH_SIZE,
            dry_run: false,
        }
    }
}

fn parse_days(name: &'static str, raw: Option<&str>, default: i64) -> Result<i64, RetentionError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    match raw.parse::<i64>() {
        Ok(days) if (1..=MAX_RETENTION_DAYS).contains(&days) => Ok(days),
        _ => Err(RetentionError::InvalidDays {
            name,
            value: raw.to_string(),
        }),
    }
}

impl RetentionPolicy {
    /// Build from raw env values (`None` or empty means the OD-RY2 default).
    pub fn from_values(
        body_days: Option<&str>,
        metadata_days: Option<&str>,
        dry_run: bool,
    ) -> Result<Self, RetentionError> {
        let policy = Self {
            body_days: parse_days(BODY_DAYS_ENV, body_days, DEFAULT_BODY_DAYS)?,
            metadata_days: parse_days(METADATA_DAYS_ENV, metadata_days, DEFAULT_METADATA_DAYS)?,
            batch_size: DEFAULT_BATCH_SIZE,
            dry_run,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), RetentionError> {
        if self.body_days > self.metadata_days {
            return Err(RetentionError::BodyLongerThanMetadata {
                body: self.body_days,
                metadata: self.metadata_days,
            });
        }
        if self.batch_size < 1 {
            return Err(RetentionError::InvalidBatch);
        }
        Ok(())
    }

    /// Window (days) that applies to a table.
    pub fn days_for(&self, table: &str) -> i64 {
        if BODY_TABLES.contains(&table) {
            self.body_days
        } else {
            self.metadata_days
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableOutcome {
    pub table: String,
    pub retention_days: i64,
    /// `false` when the table does not exist in this schema.
    pub present: bool,
    /// Rows deleted, or rows that would be deleted in a dry run.
    pub rows: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionReport {
    pub schema: String,
    pub dry_run: bool,
    pub tables: Vec<TableOutcome>,
}

impl RetentionReport {
    pub fn total_rows(&self) -> i64 {
        self.tables.iter().map(|table| table.rows).sum()
    }
}

/// Apply `policy` to `schema`. Bodies go first so a metadata row is never
/// removed while its body still exists.
pub async fn apply(
    pool: &PgPool,
    schema: &str,
    policy: &RetentionPolicy,
) -> Result<RetentionReport, RetentionError> {
    policy.validate()?;
    if !valid_schema_name(schema) {
        return Err(RetentionError::InvalidSchema(schema.to_string()));
    }
    // One cutoff clock for the whole run.
    let now_seconds: f64 = sqlx::query("SELECT extract(epoch FROM NOW())::float8 AS now")
        .fetch_one(pool)
        .await?
        .try_get("now")?;
    let mut tables = Vec::with_capacity(BODY_TABLES.len() + METADATA_TABLES.len());
    for table in BODY_TABLES.iter().chain(METADATA_TABLES.iter()) {
        let days = policy.days_for(table);
        let cutoff_seconds = now_seconds - (days as f64) * 86_400.0;
        let present = table_exists(pool, schema, table).await?;
        let rows = match (present, policy.dry_run) {
            (false, _) => 0,
            (true, true) => count_older(pool, schema, table, cutoff_seconds).await?,
            (true, false) => {
                delete_older(pool, schema, table, cutoff_seconds, policy.batch_size).await?
            }
        };
        tables.push(TableOutcome {
            table: (*table).to_string(),
            retention_days: days,
            present,
            rows,
        });
    }
    Ok(RetentionReport {
        schema: schema.to_string(),
        dry_run: policy.dry_run,
        tables,
    })
}

async fn table_exists(pool: &PgPool, schema: &str, table: &str) -> Result<bool, RetentionError> {
    let exists: bool = sqlx::query("SELECT to_regclass($1) IS NOT NULL AS present")
        .bind(format!("{schema}.{table}"))
        .fetch_one(pool)
        .await?
        .try_get("present")?;
    Ok(exists)
}

fn push_older(builder: &mut QueryBuilder<Postgres>, cutoff_seconds: f64) {
    builder.push(" WHERE created_at < to_timestamp(");
    builder.push_bind(cutoff_seconds);
    builder.push(")");
}

async fn count_older(
    pool: &PgPool,
    schema: &str,
    table: &str,
    cutoff_seconds: f64,
) -> Result<i64, RetentionError> {
    let mut builder = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint AS n FROM ");
    builder.push(schema);
    builder.push(".");
    builder.push(table);
    push_older(&mut builder, cutoff_seconds);
    Ok(builder.build().fetch_one(pool).await?.try_get("n")?)
}

async fn delete_older(
    pool: &PgPool,
    schema: &str,
    table: &str,
    cutoff_seconds: f64,
    batch_size: i64,
) -> Result<i64, RetentionError> {
    let mut total = 0_i64;
    loop {
        let mut builder = QueryBuilder::<Postgres>::new("DELETE FROM ");
        builder.push(schema);
        builder.push(".");
        builder.push(table);
        builder.push(" WHERE (id, created_at) IN (SELECT id, created_at FROM ");
        builder.push(schema);
        builder.push(".");
        builder.push(table);
        push_older(&mut builder, cutoff_seconds);
        builder.push(" LIMIT ");
        builder.push_bind(batch_size);
        builder.push(")");
        let deleted = builder.build().execute(pool).await?.rows_affected() as i64;
        total += deleted;
        if deleted < batch_size {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_od_ry2() {
        let policy = RetentionPolicy::from_values(None, None, false).unwrap();
        assert_eq!(policy.body_days, 14);
        assert_eq!(policy.metadata_days, 90);
        assert_eq!(policy.days_for("chat_inference_data"), 14);
        assert_eq!(policy.days_for("model_inferences"), 90);
    }

    #[test]
    fn bad_values_are_rejected() {
        assert!(matches!(
            RetentionPolicy::from_values(Some("0"), None, false),
            Err(RetentionError::InvalidDays { .. })
        ));
        assert!(matches!(
            RetentionPolicy::from_values(Some("abc"), None, false),
            Err(RetentionError::InvalidDays { .. })
        ));
        assert!(matches!(
            RetentionPolicy::from_values(Some("30"), Some("10"), false),
            Err(RetentionError::BodyLongerThanMetadata { .. })
        ));
        assert!(RetentionPolicy::from_values(Some(" "), Some("120"), true).is_ok());
    }
}

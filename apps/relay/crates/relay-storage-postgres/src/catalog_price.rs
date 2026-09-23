//! Operator retail price overrides for Relay catalog models.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

/// Largest accepted rate: $1,000,000 per million tokens. Anything above is a
/// typo (e.g. dollars entered as micro-dollars twice) rather than a price.
pub const MAX_PRICE_MICRO_USD_PER_MILLION: i64 = 1_000_000_000_000;

/// Maximum operator note length persisted with a price edit.
pub const MAX_PRICE_NOTE_CHARS: usize = 280;

/// One persisted operator price edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPriceOverride {
    pub model_id: String,
    pub input_micro_usd_per_million: i64,
    pub output_micro_usd_per_million: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

/// Validated write input for a price edit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogPriceInput {
    pub model: String,
    pub input_micro_usd_per_million: i64,
    pub output_micro_usd_per_million: i64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogPriceError {
    #[error("{0}")]
    Invalid(String),
    #[error("catalog price storage failed: {0}")]
    Store(#[from] sqlx::Error),
}

impl CatalogPriceInput {
    pub fn validate(&self) -> Result<(), CatalogPriceError> {
        if self.model.trim().is_empty() || self.model.len() > 512 {
            return Err(CatalogPriceError::Invalid(
                "`model` must be a catalog model id".into(),
            ));
        }
        for (field, value) in [
            (
                "input_micro_usd_per_million",
                self.input_micro_usd_per_million,
            ),
            (
                "output_micro_usd_per_million",
                self.output_micro_usd_per_million,
            ),
        ] {
            if !(0..=MAX_PRICE_MICRO_USD_PER_MILLION).contains(&value) {
                return Err(CatalogPriceError::Invalid(format!(
                    "`{field}` must be between 0 and {MAX_PRICE_MICRO_USD_PER_MILLION}"
                )));
            }
        }
        if self
            .note
            .as_ref()
            .is_some_and(|note| note.chars().count() > MAX_PRICE_NOTE_CHARS)
        {
            return Err(CatalogPriceError::Invalid(format!(
                "`note` must be at most {MAX_PRICE_NOTE_CHARS} characters"
            )));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PostgresCatalogPriceStore {
    pool: PgPool,
}

impl PostgresCatalogPriceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<CatalogPriceOverride>, CatalogPriceError> {
        let rows = sqlx::query(
            "SELECT model_id, input_micro_usd_per_million, output_micro_usd_per_million, note, \
             updated_by, (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_at_ms \
             FROM relay_catalog_price_overrides ORDER BY model_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(CatalogPriceOverride {
                    model_id: row.try_get("model_id")?,
                    input_micro_usd_per_million: row.try_get("input_micro_usd_per_million")?,
                    output_micro_usd_per_million: row.try_get("output_micro_usd_per_million")?,
                    note: row.try_get("note")?,
                    updated_by: row.try_get("updated_by")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                })
            })
            .collect()
    }

    /// Create or replace the price edit for one canonical model id.
    pub async fn upsert(
        &self,
        model_id: &str,
        input: &CatalogPriceInput,
        updated_by: &str,
    ) -> Result<(), CatalogPriceError> {
        input.validate()?;
        let note = input
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        sqlx::query(
            "INSERT INTO relay_catalog_price_overrides \
             (model_id, input_micro_usd_per_million, output_micro_usd_per_million, note, updated_by) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (model_id) DO UPDATE SET \
             input_micro_usd_per_million = EXCLUDED.input_micro_usd_per_million, \
             output_micro_usd_per_million = EXCLUDED.output_micro_usd_per_million, \
             note = EXCLUDED.note, updated_by = EXCLUDED.updated_by, updated_at = NOW()",
        )
        .bind(model_id)
        .bind(input.input_micro_usd_per_million)
        .bind(input.output_micro_usd_per_million)
        .bind(note)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a price edit so the deployed catalog price applies again.
    /// Returns `false` when no edit existed.
    pub async fn delete(&self, model_id: &str) -> Result<bool, CatalogPriceError> {
        let result = sqlx::query("DELETE FROM relay_catalog_price_overrides WHERE model_id = $1")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(input: i64, output: i64) -> CatalogPriceInput {
        CatalogPriceInput {
            model: "tensorzero::model_name::gpt-4o".into(),
            input_micro_usd_per_million: input,
            output_micro_usd_per_million: output,
            note: None,
        }
    }

    #[test]
    fn validation_rejects_negative_and_absurd_prices() {
        assert!(input(150_000, 600_000).validate().is_ok());
        assert!(input(-1, 0).validate().is_err());
        assert!(
            input(0, MAX_PRICE_MICRO_USD_PER_MILLION + 1)
                .validate()
                .is_err()
        );
        let mut blank = input(1, 1);
        blank.model = "  ".into();
        assert!(blank.validate().is_err());
    }
}

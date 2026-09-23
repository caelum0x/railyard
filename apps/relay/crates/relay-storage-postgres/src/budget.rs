//! Per-API-key monthly spend caps.
//!
//! A budget bounds one gateway API-key principal's month-to-date (UTC calendar
//! month) charged spend, summed from the durable `relay_cost_events` table. The
//! gateway consults [`PostgresKeyBudgetStore::check`] before running a billable
//! request so a leaked or runaway key has a hard ceiling even in postpaid,
//! meter-only deployments.

use relay_contract::Microdollars;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

/// Maximum operator note length persisted with a budget.
pub const MAX_BUDGET_NOTE_CHARS: usize = 280;

/// One configured budget plus its current month-to-date consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBudgetStatus {
    pub owner_principal: String,
    pub monthly_limit_micros: Microdollars,
    pub month_to_date_micros: Microdollars,
    pub remaining_micros: Microdollars,
    pub exceeded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

/// Result of a pre-request budget check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    /// No cap is configured for the principal.
    Unlimited,
    /// A cap exists and month-to-date spend is still below it.
    Within {
        limit: Microdollars,
        spent: Microdollars,
    },
    /// Month-to-date spend has reached or exceeded the cap.
    Exceeded {
        limit: Microdollars,
        spent: Microdollars,
    },
}

/// Validated upsert input for a budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyBudgetInput {
    pub monthly_limit_micros: i64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyBudgetError {
    #[error("{0}")]
    Invalid(String),
    #[error("budget storage failed: {0}")]
    Store(#[from] sqlx::Error),
}

impl KeyBudgetInput {
    pub fn validate(&self) -> Result<(), KeyBudgetError> {
        if self.monthly_limit_micros <= 0 {
            return Err(KeyBudgetError::Invalid(
                "`monthly_limit_micros` must be a positive integer".into(),
            ));
        }
        if self
            .note
            .as_ref()
            .is_some_and(|note| note.chars().count() > MAX_BUDGET_NOTE_CHARS)
        {
            return Err(KeyBudgetError::Invalid(format!(
                "`note` must be at most {MAX_BUDGET_NOTE_CHARS} characters"
            )));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PostgresKeyBudgetStore {
    pool: PgPool,
}

macro_rules! status_select {
    () => {
        "SELECT b.owner_principal, b.monthly_limit_micros, b.note, b.updated_by, \
     (EXTRACT(EPOCH FROM b.updated_at) * 1000)::bigint AS updated_at_ms, \
     COALESCE((SELECT SUM((e.event->>'charged_micros')::bigint) FROM relay_cost_events e \
       WHERE e.owner_principal=b.owner_principal \
       AND e.created_at >= date_trunc('month', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'),0)::bigint \
       AS month_to_date \
     FROM relay_key_budgets b"
    };
}

impl PostgresKeyBudgetStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Decide whether the principal may start another billable request.
    pub async fn check(&self, owner_principal: &str) -> Result<BudgetDecision, sqlx::Error> {
        let Some(status) = self.get(owner_principal).await? else {
            return Ok(BudgetDecision::Unlimited);
        };
        let limit = status.monthly_limit_micros;
        let spent = status.month_to_date_micros;
        if status.exceeded {
            return Ok(BudgetDecision::Exceeded { limit, spent });
        }
        Ok(BudgetDecision::Within { limit, spent })
    }

    /// The principal's budget with month-to-date consumption, if configured.
    pub async fn get(&self, owner_principal: &str) -> Result<Option<KeyBudgetStatus>, sqlx::Error> {
        sqlx::query(concat!(status_select!(), " WHERE b.owner_principal=$1"))
            .bind(owner_principal)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(decode_status)
            .transpose()
    }

    /// Every configured budget (operator scope), highest utilization first.
    pub async fn list(&self) -> Result<Vec<KeyBudgetStatus>, sqlx::Error> {
        let mut statuses =
            sqlx::query(concat!(status_select!(), " ORDER BY b.owner_principal ASC"))
                .fetch_all(&self.pool)
                .await?
                .iter()
                .map(decode_status)
                .collect::<Result<Vec<_>, _>>()?;
        statuses.sort_by(|a, b| utilization(b).total_cmp(&utilization(a)));
        Ok(statuses)
    }

    /// Create or replace the principal's monthly cap.
    pub async fn upsert(
        &self,
        owner_principal: &str,
        input: &KeyBudgetInput,
        updated_by: &str,
    ) -> Result<KeyBudgetStatus, KeyBudgetError> {
        input.validate()?;
        if owner_principal.trim().is_empty() {
            return Err(KeyBudgetError::Invalid("API key id is required".into()));
        }
        let note = input
            .note
            .as_ref()
            .map(|note| note.trim().to_string())
            .filter(|note| !note.is_empty());
        sqlx::query(
            "INSERT INTO relay_key_budgets \
             (owner_principal, monthly_limit_micros, note, updated_by) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (owner_principal) DO UPDATE SET \
             monthly_limit_micros=EXCLUDED.monthly_limit_micros, note=EXCLUDED.note, \
             updated_by=EXCLUDED.updated_by, updated_at=NOW()",
        )
        .bind(owner_principal)
        .bind(input.monthly_limit_micros)
        .bind(note)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        self.get(owner_principal)
            .await?
            .ok_or_else(|| KeyBudgetError::Invalid("budget was not persisted".into()))
    }

    /// Remove the principal's cap. Returns whether a cap existed.
    pub async fn delete(&self, owner_principal: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM relay_key_budgets WHERE owner_principal=$1")
            .bind(owner_principal)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

fn utilization(status: &KeyBudgetStatus) -> f64 {
    let limit = status.monthly_limit_micros.get();
    if limit <= 0 {
        return 0.0;
    }
    status.month_to_date_micros.get() as f64 / limit as f64
}

fn decode_status(row: &sqlx::postgres::PgRow) -> Result<KeyBudgetStatus, sqlx::Error> {
    let limit: i64 = row.try_get("monthly_limit_micros")?;
    let spent: i64 = row.try_get("month_to_date")?;
    Ok(KeyBudgetStatus {
        owner_principal: row.try_get("owner_principal")?,
        monthly_limit_micros: Microdollars::new(limit),
        month_to_date_micros: Microdollars::new(spent),
        remaining_micros: Microdollars::new(limit.saturating_sub(spent).max(0)),
        exceeded: spent >= limit,
        note: row.try_get("note")?,
        updated_by: row.try_get("updated_by")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use googletest::prelude::*;

    use super::*;

    #[gtest]
    fn budget_input_rejects_non_positive_limits_and_long_notes() {
        let zero = KeyBudgetInput {
            monthly_limit_micros: 0,
            note: None,
        };
        expect_that!(zero.validate().is_err(), eq(true));
        let long_note = KeyBudgetInput {
            monthly_limit_micros: 1,
            note: Some("x".repeat(MAX_BUDGET_NOTE_CHARS + 1)),
        };
        expect_that!(long_note.validate().is_err(), eq(true));
        let ok = KeyBudgetInput {
            monthly_limit_micros: 25_000_000,
            note: Some("AutoGTM worker".into()),
        };
        expect_that!(ok.validate().is_ok(), eq(true));
    }
}

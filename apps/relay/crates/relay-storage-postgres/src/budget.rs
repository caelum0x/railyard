//! Per-API-key monthly and daily spend caps.
//!
//! A budget bounds one gateway API-key principal's month-to-date (UTC calendar
//! month) charged spend, summed from the durable `relay_cost_events` table, and
//! optionally its day-to-date (UTC calendar day) spend, so one bad day cannot
//! burn the whole month. The
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
    /// Optional per-UTC-day cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_micros: Option<Microdollars>,
    pub day_to_date_micros: Microdollars,
    /// The monthly cap is reached.
    pub exceeded: bool,
    /// The daily cap is configured and reached.
    #[serde(default)]
    pub daily_exceeded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

/// Which cap a request ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Day,
    Month,
}

impl BudgetPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Month => "month",
        }
    }
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
    /// Spend for `period` has reached or exceeded that period's cap. The
    /// monthly cap wins when both are reached.
    Exceeded {
        period: BudgetPeriod,
        limit: Microdollars,
        spent: Microdollars,
    },
}

/// Validated upsert input for a budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyBudgetInput {
    pub monthly_limit_micros: i64,
    /// Optional per-UTC-day cap; at most the monthly cap.
    #[serde(default)]
    pub daily_limit_micros: Option<i64>,
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
        if let Some(daily) = self.daily_limit_micros {
            if daily <= 0 {
                return Err(KeyBudgetError::Invalid(
                    "`daily_limit_micros` must be a positive integer".into(),
                ));
            }
            if daily > self.monthly_limit_micros {
                return Err(KeyBudgetError::Invalid(
                    "`daily_limit_micros` must not exceed `monthly_limit_micros`".into(),
                ));
            }
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
        "SELECT b.owner_principal, b.monthly_limit_micros, b.daily_limit_micros, b.note, \
     b.updated_by, (EXTRACT(EPOCH FROM b.updated_at) * 1000)::bigint AS updated_at_ms, \
     COALESCE(s.month_to_date,0)::bigint AS month_to_date, \
     COALESCE(s.day_to_date,0)::bigint AS day_to_date \
     FROM relay_key_budgets b \
     LEFT JOIN LATERAL (SELECT SUM((e.event->>'charged_micros')::bigint) AS month_to_date, \
       SUM((e.event->>'charged_micros')::bigint) FILTER ( \
         WHERE e.created_at >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
       ) AS day_to_date \
       FROM relay_cost_events e \
       WHERE e.owner_principal=b.owner_principal \
       AND e.created_at >= date_trunc('month', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC') s ON TRUE"
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
        Ok(decide(&status))
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
             (owner_principal, monthly_limit_micros, daily_limit_micros, note, updated_by) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (owner_principal) DO UPDATE SET \
             monthly_limit_micros=EXCLUDED.monthly_limit_micros, \
             daily_limit_micros=EXCLUDED.daily_limit_micros, note=EXCLUDED.note, \
             updated_by=EXCLUDED.updated_by, updated_at=NOW()",
        )
        .bind(owner_principal)
        .bind(input.monthly_limit_micros)
        .bind(input.daily_limit_micros)
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

/// The pre-request decision for one budget status. The monthly cap is checked
/// first so its (longer) wait is the one reported when both are reached.
fn decide(status: &KeyBudgetStatus) -> BudgetDecision {
    if status.exceeded {
        return BudgetDecision::Exceeded {
            period: BudgetPeriod::Month,
            limit: status.monthly_limit_micros,
            spent: status.month_to_date_micros,
        };
    }
    if let (true, Some(limit)) = (status.daily_exceeded, status.daily_limit_micros) {
        return BudgetDecision::Exceeded {
            period: BudgetPeriod::Day,
            limit,
            spent: status.day_to_date_micros,
        };
    }
    BudgetDecision::Within {
        limit: status.monthly_limit_micros,
        spent: status.month_to_date_micros,
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
    let daily_limit: Option<i64> = row.try_get("daily_limit_micros")?;
    let day_spent: i64 = row.try_get("day_to_date")?;
    Ok(KeyBudgetStatus {
        owner_principal: row.try_get("owner_principal")?,
        monthly_limit_micros: Microdollars::new(limit),
        month_to_date_micros: Microdollars::new(spent),
        remaining_micros: Microdollars::new(limit.saturating_sub(spent).max(0)),
        daily_limit_micros: daily_limit.map(Microdollars::new),
        day_to_date_micros: Microdollars::new(day_spent),
        exceeded: spent >= limit,
        daily_exceeded: daily_limit.is_some_and(|daily| day_spent >= daily),
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
            daily_limit_micros: None,
            note: None,
        };
        expect_that!(zero.validate().is_err(), eq(true));
        let long_note = KeyBudgetInput {
            monthly_limit_micros: 1,
            daily_limit_micros: None,
            note: Some("x".repeat(MAX_BUDGET_NOTE_CHARS + 1)),
        };
        expect_that!(long_note.validate().is_err(), eq(true));
        let ok = KeyBudgetInput {
            monthly_limit_micros: 25_000_000,
            daily_limit_micros: Some(2_000_000),
            note: Some("AutoGTM worker".into()),
        };
        expect_that!(ok.validate().is_ok(), eq(true));
    }

    #[gtest]
    fn daily_limit_must_be_positive_and_within_the_monthly_cap() {
        let input = |daily| KeyBudgetInput {
            monthly_limit_micros: 30_000_000,
            daily_limit_micros: Some(daily),
            note: None,
        };
        expect_that!(input(0).validate().is_err(), eq(true));
        expect_that!(input(-5).validate().is_err(), eq(true));
        expect_that!(input(30_000_001).validate().is_err(), eq(true));
        expect_that!(input(30_000_000).validate().is_ok(), eq(true));
    }

    fn status(month: i64, daily: Option<i64>, day: i64) -> KeyBudgetStatus {
        KeyBudgetStatus {
            owner_principal: "rally".into(),
            monthly_limit_micros: Microdollars::new(30_000_000),
            month_to_date_micros: Microdollars::new(month),
            remaining_micros: Microdollars::new((30_000_000 - month).max(0)),
            daily_limit_micros: daily.map(Microdollars::new),
            day_to_date_micros: Microdollars::new(day),
            exceeded: month >= 30_000_000,
            daily_exceeded: daily.is_some_and(|limit| day >= limit),
            note: None,
            updated_by: "operator".into(),
            updated_at_ms: 0,
        }
    }

    #[gtest]
    fn decide_reports_the_cap_that_was_reached() {
        expect_that!(
            decide(&status(1_000_000, Some(2_000_000), 500_000)),
            eq(BudgetDecision::Within {
                limit: Microdollars::new(30_000_000),
                spent: Microdollars::new(1_000_000),
            })
        );
        expect_that!(
            decide(&status(5_000_000, Some(2_000_000), 2_000_000)),
            eq(BudgetDecision::Exceeded {
                period: BudgetPeriod::Day,
                limit: Microdollars::new(2_000_000),
                spent: Microdollars::new(2_000_000),
            })
        );
        // Both reached: the monthly cap (the longer wait) is reported.
        expect_that!(
            decide(&status(30_000_000, Some(2_000_000), 3_000_000)),
            eq(BudgetDecision::Exceeded {
                period: BudgetPeriod::Month,
                limit: Microdollars::new(30_000_000),
                spent: Microdollars::new(30_000_000),
            })
        );
        // No daily cap configured: daily spend never blocks.
        expect_that!(
            decide(&status(1_000_000, None, 9_000_000)),
            eq(BudgetDecision::Within {
                limit: Microdollars::new(30_000_000),
                spent: Microdollars::new(1_000_000),
            })
        );
    }
}

/// Live-Postgres proof of the month/day sums and the upsert. Skipped (reported
/// as passing) unless `RELAY_TEST_DATABASE_URL` or `DATABASE_URL` is set.
#[cfg(test)]
mod pg_live_tests {
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::*;

    fn db_url() -> Option<String> {
        std::env::var("RELAY_TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .filter(|value| !value.trim().is_empty())
    }

    /// When a test cost event was charged.
    #[derive(Clone, Copy)]
    enum At {
        Now = 0,
        /// One hour before today's UTC midnight; skipped on the 1st of a month.
        EarlierThisMonth = 1,
        /// One hour before this month's UTC start.
        LastMonth = 2,
    }

    async fn insert_event(pool: &PgPool, principal: &str, micros: i64, at: At) {
        sqlx::query(
            "INSERT INTO relay_cost_events (event_id, request_id, owner_principal, event, created_at) \
             SELECT $1, $2, $3, jsonb_build_object('charged_micros', $4::bigint), t.at FROM \
             (SELECT CASE $5::int \
                WHEN 0 THEN NOW() \
                WHEN 1 THEN (date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC') - INTERVAL '1 hour' \
                ELSE (date_trunc('month', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC') - INTERVAL '1 hour' \
              END AS at) t \
             WHERE $5::int <> 1 \
                OR t.at >= date_trunc('month', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'",
        )
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(principal)
        .bind(micros)
        .bind(at as i32)
        .execute(pool)
        .await
        .expect("insert cost event");
    }

    #[tokio::test]
    async fn pg_daily_and_monthly_caps_sum_the_right_windows() {
        let Some(url) = db_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to the live test Postgres");
        crate::migrate(&pool).await.expect("apply relay migrations");
        let principal = format!("budget-test-{}", Uuid::now_v7());
        let store = PostgresKeyBudgetStore::new(pool.clone());

        // Today, earlier this month (skipped on the 1st), and last month.
        insert_event(&pool, &principal, 1_500_000, At::Now).await;
        insert_event(&pool, &principal, 4_000_000, At::EarlierThisMonth).await;
        insert_event(&pool, &principal, 90_000_000, At::LastMonth).await;
        let earlier_this_month: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM((event->>'charged_micros')::bigint),0)::bigint \
             FROM relay_cost_events WHERE owner_principal=$1 \
             AND created_at >= date_trunc('month', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
             AND created_at < date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'",
        )
        .bind(&principal)
        .fetch_one(&pool)
        .await
        .unwrap();

        let input = KeyBudgetInput {
            monthly_limit_micros: 30_000_000,
            daily_limit_micros: Some(2_000_000),
            note: Some("live test".into()),
        };
        let status = store.upsert(&principal, &input, "test").await.unwrap();
        assert_eq!(status.day_to_date_micros.get(), 1_500_000);
        assert_eq!(
            status.month_to_date_micros.get(),
            1_500_000 + earlier_this_month
        );
        assert_eq!(
            status.daily_limit_micros,
            Some(Microdollars::new(2_000_000))
        );
        assert!(!status.daily_exceeded);
        assert!(matches!(
            store.check(&principal).await.unwrap(),
            BudgetDecision::Within { .. }
        ));

        insert_event(&pool, &principal, 600_000, At::Now).await;
        assert_eq!(
            store.check(&principal).await.unwrap(),
            BudgetDecision::Exceeded {
                period: BudgetPeriod::Day,
                limit: Microdollars::new(2_000_000),
                spent: Microdollars::new(2_100_000),
            }
        );

        // Replacing the budget without a daily cap clears it.
        let monthly_only = KeyBudgetInput {
            monthly_limit_micros: 30_000_000,
            daily_limit_micros: None,
            note: None,
        };
        let status = store
            .upsert(&principal, &monthly_only, "test")
            .await
            .unwrap();
        assert_eq!(status.daily_limit_micros, None);
        assert!(matches!(
            store.check(&principal).await.unwrap(),
            BudgetDecision::Within { .. }
        ));

        store.delete(&principal).await.unwrap();
        sqlx::query("DELETE FROM relay_cost_events WHERE owner_principal=$1")
            .bind(&principal)
            .execute(&pool)
            .await
            .unwrap();
    }
}

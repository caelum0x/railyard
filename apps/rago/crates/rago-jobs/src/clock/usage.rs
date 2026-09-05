//! Usage-aggregation clocks.
//!
//! Ports `Clock::{RefreshLifetimeUsages, ComputeAllDailyUsages}Job`.

use chrono::Utc;
use rago_core::enums::FeeType;
use rago_db::Db;

use crate::JobError;

/// `Clock::RefreshLifetimeUsagesJob` → `LifetimeUsages::RecalculateAndCheckJob`.
/// Recomputes each subscription's invoiced lifetime usage (sum of charge fees)
/// and upserts the accumulator, preserving the live current-period usage.
/// Idempotent. Returns the number of accumulators written.
pub async fn refresh_lifetime_usages(db: &Db) -> Result<u64, JobError> {
    let r = sqlx::query(
        "INSERT INTO lifetime_usages \
             (id, organization_id, subscription_external_id, invoiced_usage_amount_cents, current_usage_amount_cents, updated_at) \
         SELECT gen_random_uuid(), s.organization_id, s.external_id, \
                COALESCE(SUM(f.amount_cents), 0)::bigint, 0, now() \
         FROM subscriptions s \
         LEFT JOIN fees f ON f.subscription_id = s.id AND f.fee_type = $1 \
         GROUP BY s.organization_id, s.external_id \
         ON CONFLICT (organization_id, subscription_external_id) \
         DO UPDATE SET invoiced_usage_amount_cents = EXCLUDED.invoiced_usage_amount_cents, updated_at = now()",
    )
    .bind(FeeType::Charge.as_i32())
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::ComputeAllDailyUsagesJob` → `DailyUsages::ComputeAllService`. Snapshots
/// the day's usage (sum of event amounts) per subscription into `daily_usages`.
/// Idempotent per day (upsert on `(org, subscription, date)`). Returns the number
/// of snapshots written.
pub async fn compute_all_daily_usages(db: &Db) -> Result<u64, JobError> {
    let today = Utc::now().date_naive();
    let r = sqlx::query(
        "INSERT INTO daily_usages \
             (id, organization_id, subscription_external_id, usage_date, amount_cents, refreshed_at, created_at) \
         SELECT gen_random_uuid(), e.organization_id, e.external_subscription_id, $1::date, \
                COALESCE(SUM(e.precise_total_amount_cents), 0)::bigint, now(), now() \
         FROM events e \
         WHERE e.external_subscription_id IS NOT NULL AND e.deleted_at IS NULL \
           AND e.timestamp::date = $1::date \
         GROUP BY e.organization_id, e.external_subscription_id \
         ON CONFLICT (organization_id, subscription_external_id, usage_date) \
         DO UPDATE SET amount_cents = EXCLUDED.amount_cents, refreshed_at = now()",
    )
    .bind(today)
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

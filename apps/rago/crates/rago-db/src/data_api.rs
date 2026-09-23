//! Data API breakdowns backing the GraphQL `data_api_*` queries: MRR and
//! revenue by plan / customer, and usage from the `daily_usages` snapshots the
//! `compute_daily_usage` clock job maintains. All org-scoped, finalized
//! invoices only (`status = 1`).

use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// `(key, currency, amount_cents, count)`.
pub type DataPoint = (String, String, i64, i64);

fn rows_to_points(rows: &[sqlx::postgres::PgRow]) -> Vec<DataPoint> {
    rows.iter()
        .map(|r| {
            (
                r.get::<String, _>("k"),
                r.get::<String, _>("currency"),
                r.get::<Option<i64>, _>("amount").unwrap_or(0),
                r.get::<Option<i64>, _>("n").unwrap_or(0),
            )
        })
        .collect()
}

impl Db {
    /// Subscription-fee MRR per plan code.
    pub async fn data_api_mrrs_by_plan(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT p.code AS k, f.amount_currency AS currency, SUM(f.amount_cents)::bigint AS amount, \
                    COUNT(DISTINCT s.id)::bigint AS n \
             FROM fees f JOIN invoices i ON i.id = f.invoice_id \
             JOIN subscriptions s ON s.id = f.subscription_id JOIN plans p ON p.id = s.plan_id \
             WHERE i.organization_id = $1 AND i.status = 1 AND f.fee_type = 2 \
               AND COALESCE(i.issuing_date, i.created_at::date) >= date_trunc('month', now())::date - INTERVAL '1 month' \
             GROUP BY p.code, f.amount_currency ORDER BY amount DESC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Finalized revenue per customer (external id).
    pub async fn data_api_revenue_by_customer(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT c.external_id AS k, COALESCE(i.currency, 'USD') AS currency, \
                    SUM(i.total_amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM invoices i JOIN customers c ON c.id = i.customer_id \
             WHERE i.organization_id = $1 AND i.status = 1 \
             GROUP BY c.external_id, COALESCE(i.currency, 'USD') ORDER BY amount DESC LIMIT 500",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Finalized revenue (all fee types) per plan code.
    pub async fn data_api_revenue_by_plan(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT p.code AS k, f.amount_currency AS currency, SUM(f.amount_cents)::bigint AS amount, \
                    COUNT(*)::bigint AS n \
             FROM fees f JOIN invoices i ON i.id = f.invoice_id \
             JOIN subscriptions s ON s.id = f.subscription_id JOIN plans p ON p.id = s.plan_id \
             WHERE i.organization_id = $1 AND i.status = 1 \
             GROUP BY p.code, f.amount_currency ORDER BY amount DESC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Daily usage amounts by day (last 90 days).
    pub async fn data_api_usage_by_day(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(du.usage_date, 'YYYY-MM-DD') AS k, COALESCE(p.amount_currency, 'USD') AS currency, \
                    SUM(du.amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM daily_usages du \
             LEFT JOIN subscriptions s ON s.organization_id = du.organization_id AND s.external_id = du.subscription_external_id \
             LEFT JOIN plans p ON p.id = s.plan_id \
             WHERE du.organization_id = $1 AND du.usage_date >= (now() - INTERVAL '90 days')::date \
             GROUP BY k, currency ORDER BY k",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Usage amounts aggregated per subscription (current month).
    pub async fn data_api_usage_by_subscription(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT du.subscription_external_id AS k, COALESCE(p.amount_currency, 'USD') AS currency, \
                    SUM(du.amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM daily_usages du \
             LEFT JOIN subscriptions s ON s.organization_id = du.organization_id AND s.external_id = du.subscription_external_id \
             LEFT JOIN plans p ON p.id = s.plan_id \
             WHERE du.organization_id = $1 AND du.usage_date >= date_trunc('month', now())::date \
             GROUP BY k, currency ORDER BY amount DESC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Month-end usage forecast: month-to-date usage extrapolated linearly over
    /// the days of the month (key = `YYYY-MM-01`).
    pub async fn data_api_usage_forecast(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(date_trunc('month', now()), 'YYYY-MM-01') AS k, COALESCE(p.amount_currency, 'USD') AS currency, \
                    (SUM(du.amount_cents)::numeric \
                      / GREATEST(EXTRACT(DAY FROM now())::numeric, 1) \
                      * EXTRACT(DAY FROM (date_trunc('month', now()) + INTERVAL '1 month - 1 day'))::numeric)::bigint AS amount, \
                    COUNT(DISTINCT du.subscription_external_id)::bigint AS n \
             FROM daily_usages du \
             LEFT JOIN subscriptions s ON s.organization_id = du.organization_id AND s.external_id = du.subscription_external_id \
             LEFT JOIN plans p ON p.id = s.plan_id \
             WHERE du.organization_id = $1 AND du.usage_date >= date_trunc('month', now())::date \
             GROUP BY currency",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }

    /// Invoiced usage (charge fees) by month.
    pub async fn data_api_invoiced_usage(&self, org: Uuid) -> Result<Vec<DataPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(COALESCE(i.issuing_date, i.created_at::date), 'YYYY-MM-01') AS k, f.amount_currency AS currency, \
                    SUM(f.amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM fees f JOIN invoices i ON i.id = f.invoice_id \
             WHERE i.organization_id = $1 AND i.status = 1 AND f.fee_type = 0 \
             GROUP BY k, f.amount_currency ORDER BY k",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows_to_points(&rows))
    }
}

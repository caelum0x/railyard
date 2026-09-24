//! Persistence for the customer's last known IP (Iyzico buyer IP for charges
//! without a live request) and customer-requested cancellation at the end of
//! the current billing period.

use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

impl Db {
    /// Remember `ip` as the customer's last known public IP (captured on a
    /// portal or checkout request from the customer's browser).
    pub async fn record_customer_ip(&self, customer_id: Uuid, ip: &str) -> Result<()> {
        sqlx::query(
            "UPDATE customers SET last_known_ip = $2, last_known_ip_at = now() \
             WHERE id = $1 AND (last_known_ip IS DISTINCT FROM $2 \
                OR last_known_ip_at IS NULL OR last_known_ip_at < now() - interval '1 hour')",
        )
        .bind(customer_id)
        .bind(ip)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The customer's last known IP and when it was seen, if any.
    pub async fn customer_last_ip(&self, customer_id: Uuid) -> Result<Option<(String, DateTime<Utc>)>> {
        let row = sqlx::query("SELECT last_known_ip, last_known_ip_at FROM customers WHERE id = $1")
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else { return Ok(None) };
        let ip: Option<String> = r.try_get("last_known_ip")?;
        let at: Option<NaiveDateTime> = r.try_get("last_known_ip_at")?;
        Ok(ip.filter(|i| !i.is_empty()).map(|ip| (ip, at.unwrap_or_default().and_utc())))
    }

    /// Schedule an active subscription to end at `ending_at` (the end of its
    /// current period) at the customer's request. Refused (`false`) when the
    /// subscription is not active or already has an end date the customer did
    /// not set.
    pub async fn schedule_cancel_at_period_end(&self, subscription_id: Uuid, ending_at: NaiveDateTime) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE subscriptions SET ending_at = $2, cancel_at_period_end = true, updated_at = now() \
             WHERE id = $1 AND status = 1 AND (ending_at IS NULL OR cancel_at_period_end)",
        )
        .bind(subscription_id)
        .bind(ending_at)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Undo a customer-requested cancellation that has not taken effect yet.
    pub async fn undo_cancel_at_period_end(&self, subscription_id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE subscriptions SET ending_at = NULL, cancel_at_period_end = false, updated_at = now() \
             WHERE id = $1 AND status = 1 AND cancel_at_period_end AND ending_at > now()",
        )
        .bind(subscription_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// When a customer-requested cancellation takes effect, if one is pending.
    pub async fn pending_cancel_at_period_end(&self, subscription_id: Uuid) -> Result<Option<DateTime<Utc>>> {
        let row = sqlx::query(
            "SELECT ending_at FROM subscriptions \
             WHERE id = $1 AND status = 1 AND cancel_at_period_end AND ending_at IS NOT NULL",
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<NaiveDateTime, _>("ending_at").and_utc()))
    }
}

//! Customer actions pending on a payment (Stripe SCA / 3-D Secure): the
//! provider's `next_action` and the confirm link the customer must open.

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A payment waiting for the customer to authenticate.
#[derive(Debug, Clone)]
pub struct PaymentActionRow {
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub organization_id: Uuid,
    pub provider: String,
    pub provider_payment_id: Option<String>,
    pub provider_status: Option<String>,
    pub status: String,
    pub next_action: Value,
    pub next_action_url: Option<String>,
}

fn action_row(r: &sqlx::postgres::PgRow) -> Result<PaymentActionRow> {
    Ok(PaymentActionRow {
        payment_id: r.try_get("id")?,
        invoice_id: r.try_get("invoice_id")?,
        organization_id: r.try_get("organization_id")?,
        provider: r.try_get("provider")?,
        provider_payment_id: r.try_get("provider_payment_id")?,
        provider_status: r.try_get("provider_status")?,
        status: r.try_get("status")?,
        next_action: r.try_get::<Option<Value>, _>("next_action")?.unwrap_or(Value::Null),
        next_action_url: r.try_get("next_action_url")?,
    })
}

impl Db {
    /// Store (or clear, with `None`) the pending customer action of a payment.
    pub async fn set_payment_next_action(
        &self,
        payment_id: Uuid,
        next_action: Option<&Value>,
        next_action_url: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE payments SET next_action = $2, next_action_url = $3, updated_at = now() WHERE id = $1",
        )
        .bind(payment_id)
        .bind(next_action)
        .bind(next_action_url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Store the confirm link of a payment already awaiting action.
    pub async fn set_payment_next_action_url(&self, payment_id: Uuid, url: &str) -> Result<()> {
        sqlx::query("UPDATE payments SET next_action_url = $2, updated_at = now() WHERE id = $1")
            .bind(payment_id)
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record a provider-reported `requires_action` against the payment with
    /// this provider id. Returns the updated row (`None` when unknown).
    pub async fn mark_payment_requires_action(
        &self,
        provider_payment_id: &str,
        next_action: &Value,
        next_action_url: Option<&str>,
    ) -> Result<Option<PaymentActionRow>> {
        let row = sqlx::query(
            "UPDATE payments SET provider_status = 'requires_action', next_action = $2, \
             next_action_url = COALESCE($3, next_action_url), updated_at = now() \
             WHERE provider_payment_id = $1 AND status NOT IN ('succeeded', 'refunded') \
             RETURNING id, invoice_id, organization_id, provider, provider_payment_id, \
                       provider_status, status, next_action, next_action_url",
        )
        .bind(provider_payment_id)
        .bind(next_action)
        .bind(next_action_url)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(action_row).transpose()
    }

    /// Set only the provider status (e.g. `processing`) of a payment.
    pub async fn set_payment_provider_status(
        &self,
        provider_payment_id: &str,
        provider_status: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE payments SET provider_status = $2, updated_at = now() \
             WHERE provider_payment_id = $1 AND status NOT IN ('succeeded', 'refunded')",
        )
        .bind(provider_payment_id)
        .bind(provider_status)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The action pending on an invoice's payment, if the customer still has
    /// to authenticate it.
    pub async fn invoice_payment_action(&self, invoice_id: Uuid) -> Result<Option<PaymentActionRow>> {
        let row = sqlx::query(
            "SELECT id, invoice_id, organization_id, provider, provider_payment_id, provider_status, \
                    status, next_action, next_action_url \
             FROM payments WHERE invoice_id = $1 AND next_action IS NOT NULL \
               AND status NOT IN ('succeeded', 'refunded', 'failed') \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(invoice_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(action_row).transpose()
    }

    /// Point an invoice's payment at the provider payment that actually
    /// settled it (a Checkout payment completing a 3DS fallback creates a new
    /// PaymentIntent). Returns the payment id when a row was rebound.
    pub async fn rebind_invoice_payment(
        &self,
        payment_id: Uuid,
        invoice_id: Uuid,
        provider_payment_id: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE payments SET provider_payment_id = $3, next_action = NULL, updated_at = now() \
             WHERE id = $1 AND invoice_id = $2 AND status NOT IN ('succeeded', 'refunded')",
        )
        .bind(payment_id)
        .bind(invoice_id)
        .bind(provider_payment_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Clear the pending action once the payment settles.
    pub async fn clear_payment_next_action_by_provider_id(&self, provider_payment_id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE payments SET next_action = NULL, updated_at = now() WHERE provider_payment_id = $1",
        )
        .bind(provider_payment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

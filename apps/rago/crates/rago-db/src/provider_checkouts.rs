//! Hosted-checkout payment bookkeeping (Creem, Dodo Payments, Iyzico): moving
//! a payment from its checkout/session/token reference to the provider's
//! settled payment id, and replay protection for provider webhook deliveries.

use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A payment located by one of its provider references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPayment {
    pub payment_id: Uuid,
    pub invoice_id: Uuid,
    pub organization_id: Uuid,
    pub provider: String,
    pub provider_payment_id: Option<String>,
    pub status: String,
}

fn checkout_payment(r: &sqlx::postgres::PgRow) -> Result<CheckoutPayment> {
    Ok(CheckoutPayment {
        payment_id: r.try_get("id")?,
        invoice_id: r.try_get("invoice_id")?,
        organization_id: r.try_get("organization_id")?,
        provider: r.try_get("provider")?,
        provider_payment_id: r.try_get("provider_payment_id")?,
        status: r.try_get("status")?,
    })
}

impl Db {
    /// The payment whose current provider id OR original checkout reference is
    /// `reference`, restricted to `organization_id`.
    pub async fn find_payment_by_provider_reference(
        &self,
        organization_id: Uuid,
        reference: &str,
    ) -> Result<Option<CheckoutPayment>> {
        let row = sqlx::query(
            "SELECT id, invoice_id, organization_id, provider, provider_payment_id, status \
             FROM payments \
             WHERE organization_id = $1 AND (provider_payment_id = $2 OR provider_checkout_id = $2) \
             ORDER BY (provider_payment_id = $2) DESC, updated_at DESC LIMIT 1",
        )
        .bind(organization_id)
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(checkout_payment).transpose()
    }

    /// Rebind payment `payment_id` (of `organization_id`) to the provider's
    /// settled payment id, keeping the checkout reference it was created under
    /// in `provider_checkout_id`. A no-op when it is already bound to
    /// `settled_id`. Returns whether a row changed.
    pub async fn bind_checkout_to_settled_payment(
        &self,
        organization_id: Uuid,
        payment_id: Uuid,
        settled_id: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE payments SET \
                provider_checkout_id = COALESCE(provider_checkout_id, provider_payment_id), \
                provider_payment_id = $3, updated_at = now() \
             WHERE id = $1 AND organization_id = $2 \
               AND provider_payment_id IS DISTINCT FROM $3",
        )
        .bind(payment_id)
        .bind(organization_id)
        .bind(settled_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Record a processed webhook delivery. `false` when this
    /// (organization, provider, event id) was already processed.
    pub async fn record_provider_webhook_event(
        &self,
        organization_id: Uuid,
        provider: &str,
        event_id: &str,
        event_type: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO provider_webhook_events (organization_id, provider, event_id, event_type) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(organization_id)
        .bind(provider)
        .bind(event_id)
        .bind(event_type)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Forget a delivery id so the provider's retry is processed again (used
    /// when applying the event failed after it was recorded).
    pub async fn forget_provider_webhook_event(
        &self,
        organization_id: Uuid,
        provider: &str,
        event_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM provider_webhook_events \
             WHERE organization_id = $1 AND provider = $2 AND event_id = $3",
        )
        .bind(organization_id)
        .bind(provider)
        .bind(event_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

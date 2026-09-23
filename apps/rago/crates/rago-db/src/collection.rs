//! Repository support for production payment collection and webhook delivery:
//! provider credentials (sealed), provider customers + payment methods, payment
//! attempts, organization HMAC keys, endpoint fan-out, delivery-feed updates,
//! and once-per-day daemon claims.

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;
use crate::secrets;

/// A payment provider configuration with its opened secret (if one is stored
/// and the sealing key is available).
#[derive(Debug, Clone)]
pub struct ProviderCredentials {
    pub id: Uuid,
    pub provider_type: String,
    pub code: String,
    pub secret: Option<String>,
    pub success_redirect_url: Option<String>,
    pub settings: Value,
}

/// A payment row's recorded state.
#[derive(Debug, Clone)]
pub struct PaymentAttemptState {
    pub status: String,
    pub attempts: i32,
}

impl Db {
    // ---- provider credentials -------------------------------------------

    /// Seal and store a provider secret on a `payment_providers` row.
    pub async fn store_payment_provider_secret(
        &self,
        provider_id: Uuid,
        secret: &str,
    ) -> std::result::Result<(), StoreSecretError> {
        let sealed = secrets::seal(secret).map_err(StoreSecretError::Secret)?;
        sqlx::query(
            "UPDATE payment_providers SET encrypted_secrets = $2, secret_ref = $3, updated_at = now() WHERE id = $1",
        )
        .bind(provider_id)
        .bind(sealed)
        .bind(secrets::mask(secret))
        .execute(&self.pool)
        .await
        .map_err(|e| StoreSecretError::Db(e.into()))?;
        Ok(())
    }

    /// Seal and store the inbound webhook signing secret of a provider (Stripe
    /// `whsec_…`, GoCardless webhook endpoint secret, Adyen HMAC key).
    pub async fn store_payment_provider_webhook_secret(
        &self,
        provider_id: Uuid,
        secret: &str,
    ) -> std::result::Result<(), StoreSecretError> {
        let sealed = secrets::seal(secret).map_err(StoreSecretError::Secret)?;
        sqlx::query(
            "UPDATE payment_providers SET encrypted_webhook_secret = $2, webhook_secret_ref = $3, updated_at = now() WHERE id = $1",
        )
        .bind(provider_id)
        .bind(sealed)
        .bind(secrets::mask(secret))
        .execute(&self.pool)
        .await
        .map_err(|e| StoreSecretError::Db(e.into()))?;
        Ok(())
    }

    /// The opened inbound webhook secrets of the org's active providers of
    /// `provider_type` (an org may run several accounts of one provider; a
    /// signature is accepted when it verifies against any of them).
    pub async fn provider_webhook_secrets(&self, org: Uuid, provider_type: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT encrypted_webhook_secret FROM payment_providers \
             WHERE organization_id = $1 AND provider_type = $2 AND deleted_at IS NULL \
               AND encrypted_webhook_secret IS NOT NULL ORDER BY created_at",
        )
        .bind(org)
        .bind(provider_type)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let sealed: String = r.get("encrypted_webhook_secret");
                match secrets::open(&sealed) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(error = %e, "payment provider webhook secret unavailable");
                        None
                    }
                }
            })
            .collect())
    }

    /// The org's active provider of `provider_type` (or its first provider of
    /// any type when `None`), with the secret opened.
    pub async fn provider_credentials(
        &self,
        org: Uuid,
        provider_type: Option<&str>,
    ) -> Result<Option<ProviderCredentials>> {
        let row = sqlx::query(
            "SELECT id, provider_type, code, encrypted_secrets, success_redirect_url, settings \
             FROM payment_providers \
             WHERE organization_id = $1 AND deleted_at IS NULL \
               AND ($2::varchar IS NULL OR provider_type = $2) \
             ORDER BY (encrypted_secrets IS NULL), created_at LIMIT 1",
        )
        .bind(org)
        .bind(provider_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let sealed: Option<String> = r.get("encrypted_secrets");
            let secret = sealed.and_then(|s| match secrets::open(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(error = %e, "payment provider secret unavailable");
                    None
                }
            });
            ProviderCredentials {
                id: r.get("id"),
                provider_type: r.get("provider_type"),
                code: r.get("code"),
                secret,
                success_redirect_url: r.get("success_redirect_url"),
                settings: r.get("settings"),
            }
        }))
    }

    // ---- provider customers ----------------------------------------------

    /// `(provider_customer_id, payment_method_id)` for a customer + provider.
    pub async fn provider_customer(
        &self,
        customer_id: Uuid,
        provider: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let row = sqlx::query(
            "SELECT provider_customer_id, payment_method_id FROM payment_provider_customers \
             WHERE customer_id = $1 AND provider = $2 LIMIT 1",
        )
        .bind(customer_id)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("provider_customer_id"), r.get("payment_method_id"))))
    }

    /// Store the payment method used to charge a provider customer.
    pub async fn set_provider_customer_payment_method(
        &self,
        customer_id: Uuid,
        provider: &str,
        payment_method_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE payment_provider_customers SET payment_method_id = $3, updated_at = now() \
             WHERE customer_id = $1 AND provider = $2",
        )
        .bind(customer_id)
        .bind(provider)
        .bind(payment_method_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- payment attempts --------------------------------------------------

    /// The recorded state of payment `id`, if any.
    pub async fn payment_attempt_state(&self, id: Uuid) -> Result<Option<PaymentAttemptState>> {
        let row = sqlx::query("SELECT status, attempts FROM payments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| PaymentAttemptState {
            status: r.get("status"),
            attempts: r.get("attempts"),
        }))
    }

    /// Upsert the outcome of payment attempt `attempt` (0-based).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_payment_attempt(
        &self,
        id: Uuid,
        organization_id: Uuid,
        invoice_id: Uuid,
        provider: &str,
        provider_payment_id: Option<&str>,
        provider_status: Option<&str>,
        status: &str,
        amount_cents: i64,
        amount_currency: &str,
        attempt: i32,
        last_error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO payments (id, organization_id, invoice_id, provider, provider_payment_id, \
             provider_status, status, amount_cents, amount_currency, attempts, last_error, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now(), now()) \
             ON CONFLICT (id) DO UPDATE SET \
                 provider = EXCLUDED.provider, \
                 provider_payment_id = EXCLUDED.provider_payment_id, \
                 provider_status = EXCLUDED.provider_status, \
                 status = EXCLUDED.status, \
                 amount_cents = EXCLUDED.amount_cents, \
                 attempts = EXCLUDED.attempts, \
                 last_error = EXCLUDED.last_error, \
                 updated_at = now()",
        )
        .bind(id)
        .bind(organization_id)
        .bind(invoice_id)
        .bind(provider)
        .bind(provider_payment_id)
        .bind(provider_status)
        .bind(status)
        .bind(amount_cents)
        .bind(amount_currency)
        .bind(attempt)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- organization signing key ------------------------------------------

    /// The org's webhook HMAC key, generating and persisting a random one when
    /// missing (never falls back to a shared/known key).
    pub async fn ensure_organization_hmac_key(&self, org: Uuid) -> Result<String> {
        if let Some(k) = self.organization_hmac_key(org).await?.filter(|k| !k.is_empty()) {
            return Ok(k);
        }
        let fresh = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let row = sqlx::query(
            "UPDATE organizations SET hmac_key = COALESCE(NULLIF(hmac_key, ''), $2), updated_at = now() \
             WHERE id = $1 RETURNING hmac_key",
        )
        .bind(org)
        .bind(&fresh)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .and_then(|r| r.get::<Option<String>, _>("hmac_key"))
            .unwrap_or(fresh))
    }

    // ---- webhook fan-out + delivery feed ------------------------------------

    /// Every configured webhook endpoint `(id, url)` of an org.
    pub async fn webhook_endpoints_for_org(&self, org: Uuid) -> Result<Vec<(Uuid, String)>> {
        let rows = sqlx::query(
            "SELECT id, webhook_url FROM webhook_endpoints WHERE organization_id = $1 ORDER BY created_at ASC",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("webhook_url")))
            .collect())
    }

    /// Link a delivery-feed row to its `webhooks` work item.
    pub async fn link_webhook_delivery(&self, delivery_id: Uuid, webhook_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE webhook_deliveries SET webhook_id = $2 WHERE id = $1")
            .bind(delivery_id)
            .bind(webhook_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Reflect a delivery attempt of work item `webhook_id` on its feed row.
    pub async fn update_webhook_delivery_for_webhook(
        &self,
        webhook_id: Uuid,
        status: &str,
        http_status: Option<i32>,
        attempts: i32,
        last_error: Option<&str>,
        delivered: bool,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE webhook_deliveries SET status = $2, http_status = $3, attempts = $4, \
             last_error = $5, delivered_at = CASE WHEN $6 THEN now() ELSE delivered_at END \
             WHERE webhook_id = $1",
        )
        .bind(webhook_id)
        .bind(status)
        .bind(http_status)
        .bind(attempts)
        .bind(last_error)
        .bind(delivered)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Re-queue a failed delivery (operator "retry" action): the work item goes
    /// back to `pending`, due now, with its retry budget reset.
    pub async fn requeue_webhook(&self, org: Uuid, webhook_id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE webhooks SET status = 'pending', retries = 0, next_retry_at = NULL, updated_at = now() \
             WHERE id = $1 AND organization_id = $2",
        )
        .bind(webhook_id)
        .bind(org)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() > 0 {
            sqlx::query(
                "UPDATE webhook_deliveries SET status = 'pending', last_error = NULL WHERE webhook_id = $1",
            )
            .bind(webhook_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(r.rows_affected() > 0)
    }

    /// The work-item id behind a delivery-feed row (org-scoped).
    pub async fn webhook_id_for_delivery(&self, org: Uuid, delivery_id: Uuid) -> Result<Option<Uuid>> {
        let row = sqlx::query(
            "SELECT webhook_id FROM webhook_deliveries WHERE id = $1 AND organization_id = $2",
        )
        .bind(delivery_id)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.get::<Option<Uuid>, _>("webhook_id")))
    }

    // ---- daemon claims ---------------------------------------------------------

    /// Claim the `name` run for `day`. Returns `true` exactly once per day
    /// across all processes sharing the database.
    pub async fn claim_daily_run(&self, name: &str, day: NaiveDate) -> Result<bool> {
        let row = sqlx::query(
            "INSERT INTO daemon_runs (name, last_run_on, updated_at) VALUES ($1, $2, now()) \
             ON CONFLICT (name) DO UPDATE SET last_run_on = EXCLUDED.last_run_on, updated_at = now() \
             WHERE daemon_runs.last_run_on < EXCLUDED.last_run_on \
             RETURNING name",
        )
        .bind(name)
        .bind(day)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }
}

/// Failure to store a provider secret.
#[derive(Debug, thiserror::Error)]
pub enum StoreSecretError {
    #[error(transparent)]
    Secret(secrets::SecretError),
    #[error(transparent)]
    Db(crate::error::DbError),
}

impl Db {
    /// `(organization_id, resource_type)` of a data export (for jobs whose
    /// payload carries only the export id).
    pub async fn data_export_owner(&self, id: Uuid) -> Result<Option<(Uuid, String)>> {
        let row = sqlx::query("SELECT organization_id, resource_type FROM data_exports WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("organization_id"), r.get("resource_type"))))
    }

    /// Mark a data export failed (its generation job exhausted its retries or
    /// the resource type is not exportable).
    pub async fn fail_data_export(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE data_exports SET status = 'failed', updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

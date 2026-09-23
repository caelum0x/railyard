//! Integration hub persistence: full connection rows (config + opened OAuth
//! tokens), the fetched provider catalog cache (`integration_items`), resource
//! mapping lookups used to build accounting payloads, and SSO integration
//! lookups by email domain.
//!
//! Every lookup ignores soft-deleted (`deleted_at`) connections, so removing an
//! integration in the console immediately stops syncs through it.

use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// An integration connection with its settings and opened OAuth tokens.
#[derive(Debug, Clone)]
pub struct IntegrationConnection {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub integration_type: String,
    pub provider_code: String,
    pub code: Option<String>,
    pub name: Option<String>,
    pub settings: Value,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

impl IntegrationConnection {
    /// A string setting (trimmed, non-empty).
    pub fn setting(&self, key: &str) -> Option<String> {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }

    /// A boolean setting (missing = `false`).
    pub fn flag(&self, key: &str) -> bool {
        self.settings.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Open a sealed setting (`<key>_sealed`), e.g. `client_secret`.
    pub fn sealed_setting(&self, key: &str) -> Option<std::result::Result<String, crate::secrets::SecretError>> {
        self.settings
            .get(format!("{key}_sealed"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(crate::secrets::open)
    }
}

/// One cached provider catalog entry.
#[derive(Debug, Clone)]
pub struct IntegrationItem {
    pub external_id: String,
    pub item_type: String,
    pub external_name: Option<String>,
    pub external_account_code: Option<String>,
}

/// A stored resource ↔ provider mapping, with the integration it belongs to.
#[derive(Debug, Clone)]
pub struct ResourceMapping {
    pub integration_id: Uuid,
    pub integration_type: String,
    pub provider_code: String,
    pub integration_name: Option<String>,
    pub external_id: String,
    pub synced_at: NaiveDateTime,
}

fn open_token(stored: Option<String>) -> Option<String> {
    let stored = stored?;
    if !stored.starts_with("v1:") {
        return Some(stored);
    }
    match crate::secrets::open(&stored) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, "integration token could not be opened");
            None
        }
    }
}

fn connection(r: &sqlx::postgres::PgRow) -> IntegrationConnection {
    IntegrationConnection {
        id: r.get("id"),
        organization_id: r.get("organization_id"),
        integration_type: r.get("integration_type"),
        provider_code: r.get("provider_code"),
        code: r.get("code"),
        name: r.get("name"),
        settings: r.get("settings"),
        access_token: open_token(r.get("access_token")),
        refresh_token: open_token(r.get("refresh_token")),
        token_expires_at: r.get("token_expires_at"),
        created_at: r.get("created_at"),
    }
}

const CONNECTION_COLUMNS: &str = "id, organization_id, integration_type, provider_code, code, name, settings, \
     access_token, refresh_token, token_expires_at, created_at";

impl Db {
    /// A live (not soft-deleted) connection by id, scoped to `org`.
    pub async fn find_org_integration(&self, org: Uuid, id: Uuid) -> Result<Option<IntegrationConnection>> {
        let row = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE id = $1 AND organization_id = $2 AND deleted_at IS NULL"
        ))
        .bind(id)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(connection))
    }

    /// Every live connection of an organization (newest first).
    pub async fn list_org_integrations(&self, org: Uuid) -> Result<Vec<IntegrationConnection>> {
        let rows = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC"
        ))
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(connection).collect())
    }

    /// The organization's active integration of `integration_type`
    /// (`accounting`, `crm`, `tax`, `auth`): the newest live row, preferring
    /// one that holds credentials (a token or sealed settings) over an
    /// unauthorized draft.
    pub async fn active_integration_of_type(
        &self,
        org: Uuid,
        integration_type: &str,
    ) -> Result<Option<IntegrationConnection>> {
        let row = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE organization_id = $1 AND integration_type = $2 AND deleted_at IS NULL \
             ORDER BY (access_token IS NOT NULL) DESC, created_at DESC LIMIT 1"
        ))
        .bind(org)
        .bind(integration_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(connection))
    }

    /// The organization's newest live integration for `provider_code`.
    pub async fn active_integration_by_provider(
        &self,
        org: Uuid,
        provider_code: &str,
    ) -> Result<Option<IntegrationConnection>> {
        let row = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE organization_id = $1 AND provider_code = $2 AND deleted_at IS NULL \
             ORDER BY (access_token IS NOT NULL) DESC, created_at DESC LIMIT 1"
        ))
        .bind(org)
        .bind(provider_code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(connection))
    }

    /// Live SSO (`auth`) integrations serving an email `domain`, optionally
    /// restricted to one IdP.
    pub async fn auth_integrations_for_domain(
        &self,
        domain: &str,
        provider: Option<&str>,
    ) -> Result<Vec<IntegrationConnection>> {
        let rows = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE integration_type = 'auth' AND deleted_at IS NULL \
               AND lower(settings->>'domain') = lower($1) \
               AND ($2::varchar IS NULL OR provider_code = $2) \
             ORDER BY created_at DESC"
        ))
        .bind(domain.trim())
        .bind(provider)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(connection).collect())
    }

    /// Live OAuth connections whose access token expires before `horizon` and
    /// that hold a refresh token.
    pub async fn integrations_expiring_before(&self, horizon: NaiveDateTime) -> Result<Vec<IntegrationConnection>> {
        let rows = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE deleted_at IS NULL AND refresh_token IS NOT NULL AND token_expires_at IS NOT NULL \
               AND token_expires_at < $1 ORDER BY token_expires_at LIMIT 200"
        ))
        .bind(horizon)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(connection).collect())
    }

    /// Live integrations (any org) with a truthy boolean setting, e.g.
    /// `sync_invoices`.
    pub async fn integrations_with_flag(&self, integration_type: &str, flag: &str) -> Result<Vec<IntegrationConnection>> {
        let rows = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE deleted_at IS NULL AND integration_type = $1 AND (settings->>$2) = 'true' ORDER BY created_at"
        ))
        .bind(integration_type)
        .bind(flag)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(connection).collect())
    }

    /// Finalized invoices (`resource = "invoice"`) or credit notes
    /// (`resource = "credit_note"`) of an org, touched since the integration
    /// was connected, that have no sync record on it and no sync job queued
    /// for it yet. Bounded to `limit` per call.
    pub async fn unsynced_resources(
        &self,
        conn: &IntegrationConnection,
        resource: &str,
        job_type: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>> {
        let (table, id_key) = match resource {
            "credit_note" => ("credit_notes", "credit_note_id"),
            _ => ("invoices", "invoice_id"),
        };
        let sql = format!(
            "SELECT r.id FROM {table} r \
             WHERE r.organization_id = $1 AND r.status = 1 AND r.updated_at >= $2 \
               AND NOT EXISTS (SELECT 1 FROM integration_mappings m WHERE m.integration_connection_id = $3 \
                               AND m.resource_type = $4 AND m.lago_id = r.id AND m.deleted_at IS NULL) \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.job_type = $5 AND j.payload->>'{id_key}' = r.id::text \
                               AND j.payload->>'integration_id' = $3::text) \
             ORDER BY r.updated_at LIMIT $6"
        );
        let rows = sqlx::query(&sql)
            .bind(conn.organization_id)
            .bind(conn.created_at)
            .bind(conn.id)
            .bind(resource)
            .bind(job_type)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// Every live integration of `integration_type` (any org).
    pub async fn integrations_of_type(&self, integration_type: &str) -> Result<Vec<IntegrationConnection>> {
        let rows = sqlx::query(&format!(
            "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
             WHERE deleted_at IS NULL AND integration_type = $1 ORDER BY created_at"
        ))
        .bind(integration_type)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(connection).collect())
    }

    /// Finalized credit notes of `conn`'s org, issued since the tax
    /// integration was connected, against invoices that carried tax, with no
    /// tax-reporting job queued yet.
    pub async fn unreported_tax_credit_notes(
        &self,
        conn: &IntegrationConnection,
        job_type: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT cn.id FROM credit_notes cn JOIN invoices i ON i.id = cn.invoice_id \
             WHERE cn.organization_id = $1 AND cn.status = 1 AND cn.created_at >= $2 AND i.taxes_amount_cents > 0 \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.job_type = $3 AND j.payload->>'credit_note_id' = cn.id::text) \
             ORDER BY cn.created_at LIMIT $4",
        )
        .bind(conn.organization_id)
        .bind(conn.created_at)
        .bind(job_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// Replace a connection's settings document (callers merge first).
    pub async fn replace_integration_settings(&self, id: Uuid, settings: &Value) -> Result<()> {
        sqlx::query("UPDATE integration_connections SET settings = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(settings)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- fetched provider catalog -------------------------------------------

    /// Upsert one fetched catalog entry.
    pub async fn upsert_integration_item(
        &self,
        org: Uuid,
        integration_id: Uuid,
        item: &IntegrationItem,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO integration_items (id, organization_id, integration_id, external_id, item_type, external_name, external_account_code, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now()) \
             ON CONFLICT (integration_id, item_type, external_id) DO UPDATE SET \
               external_name = EXCLUDED.external_name, external_account_code = EXCLUDED.external_account_code, updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(org)
        .bind(integration_id)
        .bind(&item.external_id)
        .bind(&item.item_type)
        .bind(item.external_name.as_deref())
        .bind(item.external_account_code.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove cached entries of `item_type` that the provider no longer
    /// returns (after a full fetch).
    pub async fn prune_integration_items(
        &self,
        integration_id: Uuid,
        item_type: &str,
        keep_external_ids: &[String],
    ) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM integration_items WHERE integration_id = $1 AND item_type = $2 AND NOT (external_id = ANY($3))",
        )
        .bind(integration_id)
        .bind(item_type)
        .bind(keep_external_ids)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Cached catalog entries of one type (`item`, `account`, `subsidiary`,
    /// `tax`), or all types when `item_type` is `None`.
    pub async fn integration_items_of_type(
        &self,
        integration_id: Uuid,
        item_type: Option<&str>,
    ) -> Result<Vec<(Uuid, IntegrationItem)>> {
        let rows = sqlx::query(
            "SELECT id, external_id, item_type, external_name, external_account_code FROM integration_items \
             WHERE integration_id = $1 AND ($2::varchar IS NULL OR item_type = $2) ORDER BY item_type, external_name, external_id",
        )
        .bind(integration_id)
        .bind(item_type)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    IntegrationItem {
                        external_id: r.get("external_id"),
                        item_type: r.get::<Option<String>, _>("item_type").unwrap_or_default(),
                        external_name: r.get("external_name"),
                        external_account_code: r.get("external_account_code"),
                    },
                )
            })
            .collect())
    }

    // ---- mappings used by the sync payloads ---------------------------------

    /// The provider item mapped to a Lago object (`BillableMetric` / `AddOn`)
    /// for one integration: `(external_id, external_account_code, external_name)`.
    pub async fn object_mapping(
        &self,
        integration_id: Uuid,
        mappable_type: &str,
        mappable_id: Uuid,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        let row = sqlx::query(
            "SELECT external_id, external_account_code, external_name FROM integration_mappings \
             WHERE integration_connection_id = $1 AND lower(replace(resource_type, '_', '')) = lower(replace($2, '_', '')) \
               AND lago_id = $3 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1",
        )
        .bind(integration_id)
        .bind(mappable_type)
        .bind(mappable_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("external_id"), r.get("external_account_code"), r.get("external_name"))))
    }

    /// A collection mapping (`fallback_item`, `subscription_fee`, `coupon`,
    /// `tax`, `prepaid_credit`, `credit_note`, `minimum_commitment`, `account`)
    /// for one integration: `(external_id, external_account_code, external_name)`.
    pub async fn collection_mapping(
        &self,
        integration_id: Uuid,
        mapping_type: &str,
    ) -> Result<Option<(Option<String>, Option<String>, Option<String>)>> {
        let row = sqlx::query(
            "SELECT external_id, external_account_code, external_name FROM integration_collection_mappings \
             WHERE integration_id = $1 AND mapping_type = $2 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1",
        )
        .bind(integration_id)
        .bind(mapping_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("external_id"), r.get("external_account_code"), r.get("external_name"))))
    }

    /// The provider contact id a customer is linked to on one integration.
    pub async fn integration_customer_external_id(
        &self,
        integration_id: Uuid,
        customer_id: Uuid,
    ) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT external_customer_id FROM integration_customers \
             WHERE integration_id = $1 AND customer_id = $2 AND deleted_at IS NULL AND external_customer_id IS NOT NULL \
             ORDER BY is_default DESC, created_at DESC LIMIT 1",
        )
        .bind(integration_id)
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("external_customer_id")))
    }

    /// Integration customers linked to a customer, with their integration's
    /// provider: `(id, integration_id, provider_code, integration_type, external_customer_id)`.
    pub async fn customer_integration_links(
        &self,
        org: Uuid,
        customer_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid, String, String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT ic.id, ic.integration_id, c.provider_code, c.integration_type, ic.external_customer_id \
             FROM integration_customers ic JOIN integration_connections c ON c.id = ic.integration_id \
             WHERE ic.organization_id = $1 AND ic.customer_id = $2 AND ic.deleted_at IS NULL AND c.deleted_at IS NULL \
             ORDER BY ic.created_at",
        )
        .bind(org)
        .bind(customer_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    r.get("integration_id"),
                    r.get("provider_code"),
                    r.get("integration_type"),
                    r.get("external_customer_id"),
                )
            })
            .collect())
    }

    /// Customers linked on one integration:
    /// `(link_id, customer_id, customer_external_id, customer_name, provider_customer_id)`.
    pub async fn integration_customers_of(
        &self,
        integration_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid, String, Option<String>, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT ic.id, ic.customer_id, c.external_id, c.name, ic.external_customer_id \
             FROM integration_customers ic JOIN customers c ON c.id = ic.customer_id \
             WHERE ic.integration_id = $1 AND ic.deleted_at IS NULL ORDER BY ic.created_at DESC LIMIT 500",
        )
        .bind(integration_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("customer_id"), r.get("external_id"), r.get("name"), r.get("external_customer_id")))
            .collect())
    }

    /// Record a sync result for a Lago resource (`invoice`, `credit_note`,
    /// `customer`, `payment`) on one integration.
    pub async fn record_resource_sync(
        &self,
        org: Uuid,
        integration_id: Uuid,
        resource_type: &str,
        lago_id: Uuid,
        external_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO integration_mappings (id, organization_id, integration_connection_id, resource_type, lago_id, external_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,now(),now())",
        )
        .bind(Uuid::new_v4())
        .bind(org)
        .bind(integration_id)
        .bind(resource_type)
        .bind(lago_id)
        .bind(external_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every sync record of a Lago resource across the org's live
    /// integrations (newest first).
    pub async fn resource_mappings(
        &self,
        org: Uuid,
        resource_type: &str,
        lago_id: Uuid,
    ) -> Result<Vec<ResourceMapping>> {
        let rows = sqlx::query(
            "SELECT m.integration_connection_id, c.integration_type, c.provider_code, c.name, m.external_id, m.created_at \
             FROM integration_mappings m JOIN integration_connections c ON c.id = m.integration_connection_id \
             WHERE m.organization_id = $1 AND m.resource_type = $2 AND m.lago_id = $3 \
               AND m.deleted_at IS NULL AND c.deleted_at IS NULL \
             ORDER BY m.created_at DESC",
        )
        .bind(org)
        .bind(resource_type)
        .bind(lago_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ResourceMapping {
                integration_id: r.get("integration_connection_id"),
                integration_type: r.get("integration_type"),
                provider_code: r.get("provider_code"),
                integration_name: r.get("name"),
                external_id: r.get("external_id"),
                synced_at: r.get("created_at"),
            })
            .collect())
    }

    /// A dashboard session pinned to one organization (SSO JWT `org` claim):
    /// the organization and the user's email, only while the user is still an
    /// active member of it.
    pub async fn resolve_member_session(
        &self,
        user_id: Uuid,
        org: Uuid,
    ) -> Result<Option<(rago_core::models::Organization, String)>> {
        let Some((_, email)) = self.find_user(user_id).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT o.* FROM organizations o JOIN memberships m ON m.organization_id = o.id \
             WHERE m.user_id = $1 AND o.id = $2 AND m.status = 'active' LIMIT 1",
        )
        .bind(user_id)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some((crate::mapping::organization(&r)?, email))),
            None => Ok(None),
        }
    }

    /// Whether an active membership links `user_id` to `org`.
    pub async fn is_active_member(&self, org: Uuid, user_id: Uuid) -> Result<bool> {
        let row = sqlx::query(
            "SELECT 1 AS one FROM memberships WHERE organization_id = $1 AND user_id = $2 AND status = 'active' LIMIT 1",
        )
        .bind(org)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// The organization of an active membership for `user_id` (oldest first).
    pub async fn organizations_of_user(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT organization_id FROM memberships WHERE user_id = $1 AND status = 'active' ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("organization_id")).collect())
    }
}

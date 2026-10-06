//! Storage behind the Lago REST routes added for parity (round 2): metadata,
//! customer wallets by code, entitlements by scope, subscription overrides,
//! wallet transaction funding, API and security logs.

use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A resource that carries key/value metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataOwner {
    Plan,
    Wallet,
    CreditNote,
}

impl MetadataOwner {
    fn table(self) -> &'static str {
        match self {
            MetadataOwner::Plan => "plans",
            MetadataOwner::Wallet => "wallets",
            MetadataOwner::CreditNote => "credit_notes",
        }
    }
}

/// Where an entitlement lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementScope {
    Plan(Uuid),
    Subscription(Uuid),
}

impl EntitlementScope {
    fn column(self) -> (&'static str, Uuid) {
        match self {
            EntitlementScope::Plan(id) => ("plan_id", id),
            EntitlementScope::Subscription(id) => ("subscription_id", id),
        }
    }
}

/// One privilege value of an entitlement, with its feature.
#[derive(Debug, Clone)]
pub struct EntitlementLine {
    pub feature_id: Uuid,
    pub feature_code: String,
    pub feature_name: Option<String>,
    pub feature_description: Option<String>,
    pub privilege_code: Option<String>,
    pub privilege_name: Option<String>,
    pub value_type: Option<String>,
    pub config: Option<Value>,
    pub value: Option<String>,
}

/// A privilege of a feature.
#[derive(Debug, Clone)]
pub struct PrivilegeRow {
    pub id: Uuid,
    pub code: String,
    pub value_type: String,
}

/// A settled wallet transaction for funding allocation.
#[derive(Debug, Clone)]
pub struct LedgerTx {
    pub id: Uuid,
    pub inbound: bool,
    pub credit_amount: Decimal,
    pub amount: Decimal,
    pub created_at: chrono::NaiveDateTime,
}

/// An API log row.
#[derive(Debug, Clone)]
pub struct ApiLogRow {
    pub request_id: Uuid,
    pub api_key_id: Option<Uuid>,
    pub api_version: String,
    pub http_method: String,
    pub http_status: i32,
    pub request_path: String,
    pub request_origin: Option<String>,
    pub client: Option<String>,
    pub request_body: Option<Value>,
    pub created_at: chrono::NaiveDateTime,
}

/// A security log row.
#[derive(Debug, Clone)]
pub struct SecurityLogRow {
    pub id: Uuid,
    pub log_type: String,
    pub log_event: String,
    pub user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub resources: Value,
    pub device_info: Value,
    pub created_at: chrono::NaiveDateTime,
}

impl Db {
    // ---- metadata --------------------------------------------------------

    /// The metadata of `owner` row `id` in `org` (`None`: no such row).
    pub async fn metadata(&self, owner: MetadataOwner, org: Uuid, id: Uuid) -> Result<Option<Value>> {
        let sql = format!(
            "SELECT metadata FROM {} WHERE id = $1 AND organization_id = $2",
            owner.table()
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Replace (`merge = false`) or merge into the metadata; returns the result.
    pub async fn write_metadata(
        &self,
        owner: MetadataOwner,
        org: Uuid,
        id: Uuid,
        value: &Value,
        merge: bool,
    ) -> Result<Option<Value>> {
        let expr = if merge { "metadata || $3" } else { "$3" };
        let sql = format!(
            "UPDATE {} SET metadata = {expr}, updated_at = now() WHERE id = $1 AND organization_id = $2 RETURNING metadata",
            owner.table()
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(id)
            .bind(org)
            .bind(value)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Remove one key (`Some`) or every key (`None`).
    pub async fn delete_metadata(
        &self,
        owner: MetadataOwner,
        org: Uuid,
        id: Uuid,
        key: Option<&str>,
    ) -> Result<Option<Value>> {
        let sql = match key {
            Some(_) => format!(
                "UPDATE {} SET metadata = metadata - $3, updated_at = now() WHERE id = $1 AND organization_id = $2 RETURNING metadata",
                owner.table()
            ),
            None => format!(
                "UPDATE {} SET metadata = '{{}}'::jsonb, updated_at = now() WHERE id = $1 AND organization_id = $2 RETURNING metadata",
                owner.table()
            ),
        };
        let q = sqlx::query_scalar(&sql).bind(id).bind(org);
        let q = match key {
            Some(k) => q.bind(k.to_string()),
            None => q,
        };
        Ok(q.fetch_optional(&self.pool).await?)
    }

    // ---- wallets by code -------------------------------------------------

    /// The customer's wallet with `code` (an active one first).
    pub async fn wallet_id_by_code(&self, customer_id: Uuid, code: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM wallets WHERE customer_id = $1 AND code = $2 ORDER BY status ASC, created_at DESC LIMIT 1",
        )
        .bind(customer_id)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Whether an active wallet of the customer already uses `code`.
    pub async fn wallet_code_in_use(&self, customer_id: Uuid, code: &str) -> Result<bool> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wallets WHERE customer_id = $1 AND code = $2 AND status = 0")
                .bind(customer_id)
                .bind(code)
                .fetch_one(&self.pool)
                .await?;
        Ok(n > 0)
    }

    pub async fn set_wallet_code(&self, id: Uuid, code: &str) -> Result<()> {
        sqlx::query("UPDATE wallets SET code = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(code)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `(code, metadata)` of a wallet.
    pub async fn wallet_code_and_metadata(&self, id: Uuid) -> Result<(Option<String>, Value)> {
        let row = sqlx::query("SELECT code, metadata FROM wallets WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| (r.get("code"), r.get("metadata")))
            .unwrap_or((None, Value::Object(Default::default()))))
    }

    // ---- entitlements ----------------------------------------------------

    /// A live privilege of a feature by code.
    pub async fn live_privilege(&self, feature_id: Uuid, code: &str) -> Result<Option<PrivilegeRow>> {
        let row = sqlx::query(
            "SELECT id, code, value_type FROM entitlement_privileges WHERE entitlement_feature_id = $1 AND code = $2 AND deleted_at IS NULL",
        )
        .bind(feature_id)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| PrivilegeRow {
            id: r.get("id"),
            code: r.get("code"),
            value_type: r.get("value_type"),
        }))
    }

    /// Remove a privilege from a feature, with every value set for it.
    pub async fn delete_privilege(&self, feature_id: Uuid, code: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE entitlement_privileges SET deleted_at = now(), updated_at = now() \
             WHERE entitlement_feature_id = $1 AND code = $2 AND deleted_at IS NULL RETURNING id",
        )
        .bind(feature_id)
        .bind(code)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = id {
            sqlx::query("UPDATE entitlement_entitlement_values SET deleted_at = now(), updated_at = now() WHERE entitlement_privilege_id = $1 AND deleted_at IS NULL")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(id.is_some())
    }

    /// The live entitlement of a feature in a scope.
    pub async fn scoped_entitlement(&self, scope: EntitlementScope, feature_id: Uuid) -> Result<Option<Uuid>> {
        let (col, id) = scope.column();
        let sql = format!(
            "SELECT id FROM entitlement_entitlements WHERE {col} = $1 AND entitlement_feature_id = $2 AND deleted_at IS NULL LIMIT 1"
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(id)
            .bind(feature_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// The live entitlement of a feature in a scope, created when missing.
    pub async fn ensure_scoped_entitlement(
        &self,
        org: Uuid,
        scope: EntitlementScope,
        feature_id: Uuid,
    ) -> Result<Uuid> {
        if let Some(id) = self.scoped_entitlement(scope, feature_id).await? {
            return Ok(id);
        }
        let (col, sid) = scope.column();
        let id = Uuid::new_v4();
        let sql = format!(
            "INSERT INTO entitlement_entitlements (id, organization_id, entitlement_feature_id, {col}, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,now(),now())"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(org)
            .bind(feature_id)
            .bind(sid)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    /// Set one privilege value of an entitlement (replacing the previous one).
    pub async fn set_entitlement_value(
        &self,
        org: Uuid,
        entitlement_id: Uuid,
        privilege_id: Uuid,
        value: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE entitlement_entitlement_values SET deleted_at = now(), updated_at = now() \
             WHERE entitlement_entitlement_id = $1 AND entitlement_privilege_id = $2 AND deleted_at IS NULL",
        )
        .bind(entitlement_id)
        .bind(privilege_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO entitlement_entitlement_values (id, organization_id, entitlement_privilege_id, entitlement_entitlement_id, value, created_at, updated_at) \
             VALUES (gen_random_uuid(),$1,$2,$3,$4,now(),now())",
        )
        .bind(org)
        .bind(privilege_id)
        .bind(entitlement_id)
        .bind(value)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove an entitlement's values except for `keep` privileges.
    pub async fn clear_entitlement_values_except(&self, entitlement_id: Uuid, keep: &[Uuid]) -> Result<()> {
        sqlx::query(
            "UPDATE entitlement_entitlement_values SET deleted_at = now(), updated_at = now() \
             WHERE entitlement_entitlement_id = $1 AND deleted_at IS NULL AND NOT (entitlement_privilege_id = ANY($2))",
        )
        .bind(entitlement_id)
        .bind(keep)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove every entitlement of a scope except for `keep` features.
    pub async fn clear_scope_entitlements_except(&self, scope: EntitlementScope, keep: &[Uuid]) -> Result<()> {
        let (col, id) = scope.column();
        let sql = format!(
            "UPDATE entitlement_entitlements SET deleted_at = now(), updated_at = now() \
             WHERE {col} = $1 AND deleted_at IS NULL AND NOT (entitlement_feature_id = ANY($2))"
        );
        sqlx::query(&sql).bind(id).bind(keep).execute(&self.pool).await?;
        Ok(())
    }

    /// Remove a feature's entitlement from a scope; `false` when there was none.
    pub async fn delete_scoped_entitlement(&self, scope: EntitlementScope, feature_id: Uuid) -> Result<bool> {
        let (col, id) = scope.column();
        let sql = format!(
            "UPDATE entitlement_entitlements SET deleted_at = now(), updated_at = now() \
             WHERE {col} = $1 AND entitlement_feature_id = $2 AND deleted_at IS NULL"
        );
        Ok(sqlx::query(&sql)
            .bind(id)
            .bind(feature_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    /// Remove one privilege value of a scope's entitlement.
    pub async fn delete_scoped_entitlement_value(
        &self,
        scope: EntitlementScope,
        feature_id: Uuid,
        privilege_id: Uuid,
    ) -> Result<bool> {
        let Some(ent) = self.scoped_entitlement(scope, feature_id).await? else {
            return Ok(false);
        };
        let done = sqlx::query(
            "UPDATE entitlement_entitlement_values SET deleted_at = now(), updated_at = now() \
             WHERE entitlement_entitlement_id = $1 AND entitlement_privilege_id = $2 AND deleted_at IS NULL",
        )
        .bind(ent)
        .bind(privilege_id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Every privilege value of a scope's entitlements (a feature without
    /// values is one line without a privilege).
    pub async fn entitlement_lines(&self, scope: EntitlementScope) -> Result<Vec<EntitlementLine>> {
        let (col, id) = scope.column();
        let sql = format!(
            "SELECT f.id AS feature_id, f.code AS feature_code, f.name AS feature_name, f.description AS feature_description, \
                    p.code AS privilege_code, p.name AS privilege_name, p.value_type, p.config, v.value \
             FROM entitlement_entitlements e \
             JOIN entitlement_features f ON f.id = e.entitlement_feature_id AND f.deleted_at IS NULL \
             LEFT JOIN entitlement_entitlement_values v ON v.entitlement_entitlement_id = e.id AND v.deleted_at IS NULL \
             LEFT JOIN entitlement_privileges p ON p.id = v.entitlement_privilege_id AND p.deleted_at IS NULL \
             WHERE e.{col} = $1 AND e.deleted_at IS NULL ORDER BY f.code, p.code"
        );
        let rows = sqlx::query(&sql).bind(id).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| EntitlementLine {
                feature_id: r.get("feature_id"),
                feature_code: r.get("feature_code"),
                feature_name: r.get("feature_name"),
                feature_description: r.get("feature_description"),
                privilege_code: r.get("privilege_code"),
                privilege_name: r.get("privilege_name"),
                value_type: r.get("value_type"),
                config: r.get("config"),
                value: r.get("value"),
            })
            .collect())
    }

    /// Record that a subscription drops a plan feature (or one privilege).
    pub async fn add_feature_removal(
        &self,
        org: Uuid,
        subscription_id: Uuid,
        feature_id: Uuid,
        privilege_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO entitlement_subscription_feature_removals (organization_id, subscription_id, entitlement_feature_id, entitlement_privilege_id) \
             VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        )
        .bind(org)
        .bind(subscription_id)
        .bind(feature_id)
        .bind(privilege_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Undo removals of a feature for a subscription (it is set again).
    pub async fn clear_feature_removals(
        &self,
        subscription_id: Uuid,
        feature_id: Uuid,
        privilege_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM entitlement_subscription_feature_removals WHERE subscription_id = $1 AND entitlement_feature_id = $2 \
             AND ($3::uuid IS NULL OR entitlement_privilege_id IS NULL OR entitlement_privilege_id = $3)",
        )
        .bind(subscription_id)
        .bind(feature_id)
        .bind(privilege_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `(feature code, privilege code or None)` removed for a subscription.
    pub async fn feature_removals(&self, subscription_id: Uuid) -> Result<Vec<(String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT f.code AS feature_code, p.code AS privilege_code FROM entitlement_subscription_feature_removals r \
             JOIN entitlement_features f ON f.id = r.entitlement_feature_id \
             LEFT JOIN entitlement_privileges p ON p.id = r.entitlement_privilege_id WHERE r.subscription_id = $1",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("feature_code"), r.get("privilege_code")))
            .collect())
    }

    // ---- subscription overrides ------------------------------------------

    /// The subscription's own copy of its plan (Lago `Plans::OverrideService`):
    /// created on first use with the plan's charges, filters, fixed charges,
    /// usage thresholds, commitments and entitlements, and assigned to the
    /// subscription. Returns the override plan's id.
    pub async fn ensure_override_plan(&self, subscription_id: Uuid) -> Result<Uuid> {
        let mut tx = self.pool.begin().await?;
        let (plan_id, parent): (Uuid, Option<Uuid>) = {
            let row = sqlx::query(
                "SELECT p.id, p.parent_id FROM subscriptions s JOIN plans p ON p.id = s.plan_id WHERE s.id = $1 FOR UPDATE OF s",
            )
            .bind(subscription_id)
            .fetch_one(&mut *tx)
            .await?;
            (row.get("id"), row.get("parent_id"))
        };
        if parent.is_some() {
            tx.commit().await?;
            return Ok(plan_id);
        }
        let new_plan = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO plans SELECT (jsonb_populate_record(NULL::plans, to_jsonb(p) || jsonb_build_object( \
                'id', $2::uuid, 'parent_id', p.id, 'created_at', now(), 'updated_at', now()))).* FROM plans p WHERE p.id = $1",
        )
        .bind(plan_id)
        .bind(new_plan)
        .execute(&mut *tx)
        .await?;
        // Charges, then their filters (mapped to the new charges).
        sqlx::query("CREATE TEMP TABLE IF NOT EXISTS override_charge_map (old_id uuid, new_id uuid) ON COMMIT DROP")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO override_charge_map SELECT id, gen_random_uuid() FROM charges WHERE plan_id = $1 AND deleted_at IS NULL")
            .bind(plan_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO charges SELECT (jsonb_populate_record(NULL::charges, to_jsonb(c) || jsonb_build_object( \
                'id', m.new_id, 'plan_id', $2::uuid, 'parent_id', c.id, 'created_at', now(), 'updated_at', now()))).* \
             FROM charges c JOIN override_charge_map m ON m.old_id = c.id",
        )
        .bind(plan_id)
        .bind(new_plan)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO charge_filters SELECT (jsonb_populate_record(NULL::charge_filters, to_jsonb(f) || jsonb_build_object( \
                'id', gen_random_uuid(), 'charge_id', m.new_id, 'created_at', f.created_at, 'updated_at', now()))).* \
             FROM charge_filters f JOIN override_charge_map m ON m.old_id = f.charge_id WHERE f.deleted_at IS NULL",
        )
        .execute(&mut *tx)
        .await?;
        for table in ["fixed_charges", "usage_thresholds", "commitments"] {
            let sql = format!(
                "INSERT INTO {table} SELECT (jsonb_populate_record(NULL::{table}, to_jsonb(t) || jsonb_build_object( \
                    'id', gen_random_uuid(), 'plan_id', $2::uuid, 'created_at', now(), 'updated_at', now()))).* \
                 FROM {table} t WHERE t.plan_id = $1 AND t.deleted_at IS NULL"
            );
            sqlx::query(&sql).bind(plan_id).bind(new_plan).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE fixed_charges f SET parent_id = o.id FROM fixed_charges o WHERE f.plan_id = $2 AND o.plan_id = $1 AND o.code = f.code AND o.deleted_at IS NULL")
            .bind(plan_id)
            .bind(new_plan)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE subscriptions SET plan_id = $2, updated_at = now() WHERE id = $1")
            .bind(subscription_id)
            .bind(new_plan)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(new_plan)
    }

    // ---- wallet transactions ---------------------------------------------

    /// One wallet transaction of the organization.
    pub async fn find_wallet_transaction(
        &self,
        org: Uuid,
        id: Uuid,
    ) -> Result<Option<rago_core::models::WalletTransaction>> {
        let row = sqlx::query("SELECT * FROM wallet_transactions WHERE id = $1 AND organization_id = $2")
            .bind(id)
            .bind(org)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(crate::mapping::wallet_transaction).transpose()
    }

    /// A wallet's settled transactions, oldest first (FIFO funding order).
    pub async fn settled_wallet_ledger(&self, wallet_id: Uuid) -> Result<Vec<LedgerTx>> {
        let rows = sqlx::query(
            "SELECT id, transaction_type, credit_amount, amount, created_at FROM wallet_transactions \
             WHERE wallet_id = $1 AND status = 1 ORDER BY created_at, id",
        )
        .bind(wallet_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| LedgerTx {
                id: r.get("id"),
                inbound: r.get::<i32, _>("transaction_type") == 0,
                credit_amount: r.get("credit_amount"),
                amount: r.get("amount"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    // ---- api and security logs -------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_api_log(
        &self,
        org: Uuid,
        request_id: Uuid,
        api_key_id: Option<Uuid>,
        api_version: &str,
        method: &str,
        status: i32,
        path: &str,
        origin: Option<&str>,
        client: Option<&str>,
        body: Option<&Value>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO api_logs (organization_id, request_id, api_key_id, api_version, http_method, http_status, request_path, request_origin, client, request_body) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (request_id) DO NOTHING",
        )
        .bind(org)
        .bind(request_id)
        .bind(api_key_id)
        .bind(api_version)
        .bind(method)
        .bind(status)
        .bind(path)
        .bind(origin)
        .bind(client)
        .bind(body)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_api_logs(&self, org: Uuid, limit: i64, offset: i64) -> Result<(Vec<ApiLogRow>, i64)> {
        let rows = sqlx::query(
            "SELECT * FROM api_logs WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_logs WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&self.pool)
            .await?;
        Ok((rows.iter().map(api_log).collect(), total))
    }

    pub async fn find_api_log(&self, org: Uuid, request_id: Uuid) -> Result<Option<ApiLogRow>> {
        let row = sqlx::query("SELECT * FROM api_logs WHERE organization_id = $1 AND request_id = $2")
            .bind(org)
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(api_log))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_security_log(
        &self,
        org: Uuid,
        log_type: &str,
        log_event: &str,
        user_id: Option<Uuid>,
        api_key_id: Option<Uuid>,
        resources: &Value,
        device_info: &Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO security_logs (organization_id, log_type, log_event, user_id, api_key_id, resources, device_info) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(org)
        .bind(log_type)
        .bind(log_event)
        .bind(user_id)
        .bind(api_key_id)
        .bind(resources)
        .bind(device_info)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_security_logs(&self, org: Uuid, limit: i64, offset: i64) -> Result<(Vec<SecurityLogRow>, i64)> {
        let rows = sqlx::query(
            "SELECT * FROM security_logs WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM security_logs WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&self.pool)
            .await?;
        Ok((rows.iter().map(security_log).collect(), total))
    }

    pub async fn find_security_log(&self, org: Uuid, id: Uuid) -> Result<Option<SecurityLogRow>> {
        let row = sqlx::query("SELECT * FROM security_logs WHERE organization_id = $1 AND id = $2")
            .bind(org)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(security_log))
    }

    /// One activity log: `(id, activity_type, source, resource_type, resource_id, ext customer, ext sub, payload, created)`.
    pub async fn find_activity_log(&self, org: Uuid, id: Uuid) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT * FROM activity_logs WHERE organization_id = $1 AND id = $2")
            .bind(org)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| activity_json(&r)))
    }

    /// Activity logs as Lago `V1::ActivityLogSerializer` objects, newest first.
    pub async fn activity_logs_json(&self, org: Uuid, limit: i64, offset: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT * FROM activity_logs WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(activity_json).collect())
    }

    // ---- lifetime usage and usage analytics -------------------------------

    /// Set the usage carried in from before Rago; returns `(invoiced, current, historical)`.
    pub async fn set_historical_usage(&self, org: Uuid, sub_ext: &str, cents: i64) -> Result<(i64, i64, i64)> {
        let row = sqlx::query(
            "INSERT INTO lifetime_usages (organization_id, subscription_external_id, historical_usage_amount_cents, updated_at) \
             VALUES ($1,$2,$3,now()) ON CONFLICT (organization_id, subscription_external_id) DO UPDATE SET \
             historical_usage_amount_cents = EXCLUDED.historical_usage_amount_cents, updated_at = now() \
             RETURNING invoiced_usage_amount_cents, current_usage_amount_cents, historical_usage_amount_cents",
        )
        .bind(org)
        .bind(sub_ext)
        .bind(cents)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.get(0), row.get(1), row.get(2)))
    }

    pub async fn historical_usage(&self, org: Uuid, sub_ext: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT historical_usage_amount_cents FROM lifetime_usages WHERE organization_id = $1 AND subscription_external_id = $2",
        )
        .bind(org)
        .bind(sub_ext)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0))
    }

    /// Daily usage per day (and subscription when given) in `[from, to]`:
    /// `(date, external subscription id, amount cents)`.
    pub async fn daily_usage(
        &self,
        org: Uuid,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
        sub_ext: Option<&str>,
    ) -> Result<Vec<(chrono::NaiveDate, String, i64)>> {
        let rows = sqlx::query(
            "SELECT usage_date, subscription_external_id, amount_cents FROM daily_usages \
             WHERE organization_id = $1 AND usage_date BETWEEN $2 AND $3 AND ($4::varchar IS NULL OR subscription_external_id = $4) \
             ORDER BY usage_date, subscription_external_id",
        )
        .bind(org)
        .bind(from)
        .bind(to)
        .bind(sub_ext)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
    }
}

fn api_log(r: &sqlx::postgres::PgRow) -> ApiLogRow {
    ApiLogRow {
        request_id: r.get("request_id"),
        api_key_id: r.get("api_key_id"),
        api_version: r.get("api_version"),
        http_method: r.get("http_method"),
        http_status: r.get("http_status"),
        request_path: r.get("request_path"),
        request_origin: r.get("request_origin"),
        client: r.get("client"),
        request_body: r.get("request_body"),
        created_at: r.get("created_at"),
    }
}

fn security_log(r: &sqlx::postgres::PgRow) -> SecurityLogRow {
    SecurityLogRow {
        id: r.get("id"),
        log_type: r.get("log_type"),
        log_event: r.get("log_event"),
        user_id: r.get("user_id"),
        api_key_id: r.get("api_key_id"),
        resources: r.get("resources"),
        device_info: r.get("device_info"),
        created_at: r.get("created_at"),
    }
}

fn activity_json(r: &sqlx::postgres::PgRow) -> Value {
    let created: chrono::NaiveDateTime = r.get("created_at");
    serde_json::json!({
        "activity_id": r.get::<Uuid, _>("id"),
        "activity_type": r.get::<String, _>("activity_type"),
        "activity_source": r.get::<String, _>("activity_source"),
        "resource_type": r.get::<Option<String>, _>("resource_type"),
        "resource_id": r.get::<Option<Uuid>, _>("resource_id"),
        "external_customer_id": r.get::<Option<String>, _>("external_customer_id"),
        "external_subscription_id": r.get::<Option<String>, _>("external_subscription_id"),
        "activity_object": r.get::<Value, _>("payload"),
        "logged_at": created.and_utc().to_rfc3339(),
    })
}

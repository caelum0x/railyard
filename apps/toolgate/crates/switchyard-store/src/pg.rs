//! Postgres [`Store`] (sqlx, runtime-checked queries, embedded migrations).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use switchyard_core::hash::{chain_hash, GENESIS_HASH};
use switchyard_core::{CapabilityManifest, ProviderManifest, ToolManifest};

use crate::models::*;
use crate::{Store, StoreError, StoreResult};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Connect with a small pool (RAM budget: 5 connections) and migrate.
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
        MIGRATOR.run(&pool).await.map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(PgStore { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        PgStore { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Mirror the YAML catalog into `providers`, `capabilities`, `tools`.
    pub async fn sync_catalog(
        &self,
        providers: &[ProviderManifest],
        capabilities: &[CapabilityManifest],
        tools: &[ToolManifest],
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        for p in providers {
            sqlx::query(
                "INSERT INTO providers (id, kind, egress_hosts, tos_resale_ok, manifest) VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (id) DO UPDATE SET kind=$2, egress_hosts=$3, tos_resale_ok=$4, manifest=$5",
            )
            .bind(&p.id)
            .bind(p.kind.as_str())
            .bind(&p.egress_hosts)
            .bind(p.tos_resale_ok)
            .bind(serde_json::to_value(p).unwrap_or(Value::Null))
            .execute(&mut *tx)
            .await?;
        }
        for c in capabilities {
            sqlx::query(
                "INSERT INTO capabilities (id, title, description, canonical_input_schema, canonical_output_schema) VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (id) DO UPDATE SET title=$2, description=$3, canonical_input_schema=$4, canonical_output_schema=$5",
            )
            .bind(&c.id)
            .bind(&c.title)
            .bind(&c.description)
            .bind(&c.input_schema)
            .bind(&c.output_schema)
            .execute(&mut *tx)
            .await?;
        }
        for t in tools {
            sqlx::query(
                "INSERT INTO tools (id, capability_id, provider_id, version, input_schema, output_schema, side_effect, price, cache_ttl_s, idempotent, status, manifest)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                 ON CONFLICT (id) DO UPDATE SET capability_id=$2, provider_id=$3, version=$4, input_schema=$5, output_schema=$6,
                   side_effect=$7, price=$8, cache_ttl_s=$9, idempotent=$10, status=$11, manifest=$12",
            )
            .bind(&t.id)
            .bind(&t.capability)
            .bind(&t.provider)
            .bind(t.version as i32)
            .bind(&t.input_schema)
            .bind(&t.output_schema)
            .bind(t.side_effect.as_str())
            .bind(serde_json::to_value(&t.price).unwrap_or(Value::Null))
            .bind(t.cache_ttl_s as i64)
            .bind(t.idempotent)
            .bind(&t.status)
            .bind(serde_json::to_value(t).unwrap_or(Value::Null))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn org(r: &PgRow) -> Org {
    Org {
        id: r.get("id"),
        slug: r.get("slug"),
        name: r.get("name"),
        mint_account_id: r.get("mint_account_id"),
        rago_subscription_id: r.get("rago_subscription_id"),
        created_at: r.get("created_at"),
    }
}

fn project(r: &PgRow) -> Project {
    let caps: Value = r.get("tool_daily_caps");
    let hosts: Value = r.get("egress_extra_hosts");
    Project {
        id: r.get("id"),
        org_id: r.get("org_id"),
        name: r.get("name"),
        monthly_cap_micros: r.get("monthly_cap_micros"),
        daily_cap_micros: r.get("daily_cap_micros"),
        tool_daily_caps: serde_json::from_value::<BTreeMap<String, i64>>(caps).unwrap_or_default(),
        routing_policy: r.get("routing_policy"),
        egress_extra_hosts: serde_json::from_value(hosts).unwrap_or_default(),
        pinned_tools: r.get("pinned_tools"),
        daily_send_limit: r.get("daily_send_limit"),
        created_at: r.get("created_at"),
    }
}

fn api_key(r: &PgRow) -> ApiKey {
    ApiKey {
        public_id: r.get("public_id"),
        project_id: r.get("project_id"),
        description: r.get("description"),
        scopes: r.get("scopes"),
        tool_allow: r.get("tool_allow"),
        per_key_daily_cap_micros: r.get("per_key_daily_cap_micros"),
        extra_projects: r.get("extra_projects"),
        revoked_at: r.get("revoked_at"),
        created_at: r.get("created_at"),
    }
}

fn org_key(r: &PgRow) -> OrgKey {
    OrgKey { org_id: r.get("org_id"), dek_wrapped: r.get("dek_wrapped"), kek_version: r.get("kek_version"), created_at: r.get("created_at") }
}

fn credential(r: &PgRow) -> CredentialRow {
    CredentialRow {
        id: r.get("id"),
        org_id: r.get("org_id"),
        provider_id: r.get("provider_id"),
        name: r.get("name"),
        kind: r.get("kind"),
        ciphertext: r.get("ciphertext"),
        nonce: r.get("nonce"),
        meta: r.get("meta"),
        created_at: r.get("created_at"),
        last_ok_at: r.get("last_ok_at"),
        last_error: r.get("last_error"),
    }
}

fn bundle(r: &PgRow) -> BundleRow {
    BundleRow {
        id: r.get("id"),
        org_id: r.get("org_id"),
        name: r.get("name"),
        description: r.get("description"),
        skill_md: r.get("skill_md"),
        tool_ids: r.get("tool_ids"),
        credential_refs: r.get("credential_refs"),
        version: r.get("version"),
        updated_at: r.get("updated_at"),
    }
}

fn byo(r: &PgRow) -> ByoTool {
    ByoTool {
        id: r.get("id"),
        org_id: r.get("org_id"),
        base_url: r.get("base_url"),
        credential_id: r.get("credential_id"),
        header: r.get("header"),
        prefix: r.get("prefix"),
        created_at: r.get("created_at"),
    }
}

fn call(r: &PgRow) -> CallRow {
    CallRow {
        call: NewCall {
            id: r.get("id"),
            project_id: r.get("project_id"),
            org_id: r.get("org_id"),
            key_public_id: r.get("key_public_id"),
            target: r.get("target"),
            tool_id: r.get("tool_id"),
            provider_id: r.get("provider_id"),
            side_effect: r.get("side_effect"),
            idempotency_key: r.get("idempotency_key"),
            request_hash: r.get("request_hash"),
            status: r.get("status"),
            http_status: r.get("http_status"),
            latency_ms: r.get("latency_ms"),
            cost_micros: r.get("cost_micros"),
            price_basis: r.get("price_basis"),
            mint_hold_id: r.get("mint_hold_id"),
            mint_receipt: r.get("mint_receipt"),
            rago_event_id: r.get("rago_event_id"),
            cache: r.get("cache"),
            fallback_chain: r.get("fallback_chain"),
            created_at: r.get("created_at"),
        },
        seq: r.get("seq"),
        prev_hash: r.get("prev_hash"),
        row_hash: r.get("row_hash"),
    }
}

fn outbox(r: &PgRow) -> OutboxItem {
    OutboxItem {
        id: r.get("id"),
        kind: r.get("kind"),
        payload: r.get("payload"),
        attempts: r.get("attempts"),
        next_attempt_at: r.get("next_attempt_at"),
        delivered_at: r.get("delivered_at"),
        last_error: r.get("last_error"),
    }
}

fn client(r: &PgRow) -> OAuthClient {
    OAuthClient {
        client_id: r.get("client_id"),
        kind: r.get("kind"),
        metadata: r.get("metadata"),
        redirect_uris: r.get("redirect_uris"),
        created_at: r.get("created_at"),
    }
}

fn grant(r: &PgRow) -> OAuthGrant {
    OAuthGrant {
        id: r.get("id"),
        family_id: r.get("family_id"),
        client_id: r.get("client_id"),
        user_id: r.get("user_id"),
        org_id: r.get("org_id"),
        project_id: r.get("project_id"),
        scopes: r.get("scopes"),
        refresh_hash: r.get("refresh_hash"),
        resource: r.get("resource"),
        expires_at: r.get("expires_at"),
        used_at: r.get("used_at"),
        created_at: r.get("created_at"),
    }
}

fn device(r: &PgRow) -> DeviceCode {
    DeviceCode {
        device_hash: r.get("device_hash"),
        user_code: r.get("user_code"),
        client_id: r.get("client_id"),
        scopes: r.get("scopes"),
        status: r.get("status"),
        user_id: r.get("user_id"),
        org_id: r.get("org_id"),
        project_id: r.get("project_id"),
        expires_at: r.get("expires_at"),
        last_poll_at: r.get("last_poll_at"),
        interval_s: r.get("interval_s"),
    }
}

const PROJECT_COLS: &str =
    "id, org_id, name, monthly_cap_micros, daily_cap_micros, tool_daily_caps, routing_policy, egress_extra_hosts, pinned_tools, daily_send_limit, created_at";

#[async_trait]
impl Store for PgStore {
    async fn create_org(&self, o: Org) -> StoreResult<Org> {
        sqlx::query("INSERT INTO orgs (id, slug, name, mint_account_id, rago_subscription_id, created_at) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(&o.id)
            .bind(&o.slug)
            .bind(&o.name)
            .bind(&o.mint_account_id)
            .bind(&o.rago_subscription_id)
            .bind(o.created_at)
            .execute(&self.pool)
            .await?;
        Ok(o)
    }
    async fn get_org(&self, id: &str) -> StoreResult<Org> {
        let r = sqlx::query("SELECT * FROM orgs WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        r.map(|r| org(&r)).ok_or_else(|| StoreError::NotFound("org".into()))
    }
    async fn list_orgs(&self) -> StoreResult<Vec<Org>> {
        Ok(sqlx::query("SELECT * FROM orgs ORDER BY created_at").fetch_all(&self.pool).await?.iter().map(org).collect())
    }
    async fn create_project(&self, p: Project) -> StoreResult<Project> {
        sqlx::query(&format!("INSERT INTO projects ({PROJECT_COLS}) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)"))
            .bind(&p.id)
            .bind(&p.org_id)
            .bind(&p.name)
            .bind(p.monthly_cap_micros)
            .bind(p.daily_cap_micros)
            .bind(serde_json::to_value(&p.tool_daily_caps).unwrap_or(Value::Null))
            .bind(&p.routing_policy)
            .bind(serde_json::to_value(&p.egress_extra_hosts).unwrap_or(Value::Null))
            .bind(&p.pinned_tools)
            .bind(p.daily_send_limit)
            .bind(p.created_at)
            .execute(&self.pool)
            .await?;
        Ok(p)
    }
    async fn get_project(&self, id: &str) -> StoreResult<Project> {
        let r = sqlx::query(&format!("SELECT {PROJECT_COLS} FROM projects WHERE id = $1")).bind(id).fetch_optional(&self.pool).await?;
        r.map(|r| project(&r)).ok_or_else(|| StoreError::NotFound("project".into()))
    }
    async fn update_project(&self, p: Project) -> StoreResult<Project> {
        let n = sqlx::query(
            "UPDATE projects SET name=$2, monthly_cap_micros=$3, daily_cap_micros=$4, tool_daily_caps=$5, routing_policy=$6,
             egress_extra_hosts=$7, pinned_tools=$8, daily_send_limit=$9 WHERE id=$1",
        )
        .bind(&p.id)
        .bind(&p.name)
        .bind(p.monthly_cap_micros)
        .bind(p.daily_cap_micros)
        .bind(serde_json::to_value(&p.tool_daily_caps).unwrap_or(Value::Null))
        .bind(&p.routing_policy)
        .bind(serde_json::to_value(&p.egress_extra_hosts).unwrap_or(Value::Null))
        .bind(&p.pinned_tools)
        .bind(p.daily_send_limit)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound("project".into()));
        }
        Ok(p)
    }
    async fn list_projects(&self, org_id: &str) -> StoreResult<Vec<Project>> {
        Ok(sqlx::query(&format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id = $1 ORDER BY created_at"))
            .bind(org_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(project)
            .collect())
    }
    async fn upsert_member(&self, m: Member) -> StoreResult<()> {
        sqlx::query("INSERT INTO members (org_id, user_id, role) VALUES ($1,$2,$3) ON CONFLICT (org_id, user_id) DO UPDATE SET role = $3")
            .bind(&m.org_id)
            .bind(&m.user_id)
            .bind(&m.role)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn get_member(&self, org_id: &str, user_id: &str) -> StoreResult<Option<Member>> {
        let r = sqlx::query("SELECT org_id, user_id, role FROM members WHERE org_id=$1 AND user_id=$2")
            .bind(org_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| Member { org_id: r.get("org_id"), user_id: r.get("user_id"), role: r.get("role") }))
    }

    async fn insert_api_key(&self, k: ApiKey) -> StoreResult<ApiKey> {
        sqlx::query(
            "INSERT INTO api_keys (public_id, project_id, description, scopes, tool_allow, per_key_daily_cap_micros, extra_projects, revoked_at, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(&k.public_id)
        .bind(&k.project_id)
        .bind(&k.description)
        .bind(&k.scopes)
        .bind(&k.tool_allow)
        .bind(k.per_key_daily_cap_micros)
        .bind(&k.extra_projects)
        .bind(k.revoked_at)
        .bind(k.created_at)
        .execute(&self.pool)
        .await?;
        Ok(k)
    }
    async fn get_api_key(&self, public_id: &str) -> StoreResult<Option<ApiKey>> {
        Ok(sqlx::query("SELECT * FROM api_keys WHERE public_id = $1").bind(public_id).fetch_optional(&self.pool).await?.map(|r| api_key(&r)))
    }
    async fn list_api_keys(&self, project_id: &str) -> StoreResult<Vec<ApiKey>> {
        Ok(sqlx::query("SELECT * FROM api_keys WHERE project_id = $1 ORDER BY created_at")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(api_key)
            .collect())
    }
    async fn update_api_key(&self, k: ApiKey) -> StoreResult<ApiKey> {
        let n = sqlx::query(
            "UPDATE api_keys SET description=$2, scopes=$3, tool_allow=$4, per_key_daily_cap_micros=$5, extra_projects=$6, revoked_at=$7 WHERE public_id=$1",
        )
        .bind(&k.public_id)
        .bind(&k.description)
        .bind(&k.scopes)
        .bind(&k.tool_allow)
        .bind(k.per_key_daily_cap_micros)
        .bind(&k.extra_projects)
        .bind(k.revoked_at)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound("api key".into()));
        }
        Ok(k)
    }

    async fn ensure_org_key(&self, k: OrgKey) -> StoreResult<OrgKey> {
        sqlx::query("INSERT INTO org_keys (org_id, dek_wrapped, kek_version, created_at) VALUES ($1,$2,$3,$4) ON CONFLICT (org_id) DO NOTHING")
            .bind(&k.org_id)
            .bind(&k.dek_wrapped)
            .bind(k.kek_version)
            .bind(k.created_at)
            .execute(&self.pool)
            .await?;
        self.get_org_key(&k.org_id).await?.ok_or_else(|| StoreError::NotFound("org key".into()))
    }
    async fn get_org_key(&self, org_id: &str) -> StoreResult<Option<OrgKey>> {
        Ok(sqlx::query("SELECT * FROM org_keys WHERE org_id = $1").bind(org_id).fetch_optional(&self.pool).await?.map(|r| org_key(&r)))
    }
    async fn list_org_keys(&self) -> StoreResult<Vec<OrgKey>> {
        Ok(sqlx::query("SELECT * FROM org_keys").fetch_all(&self.pool).await?.iter().map(org_key).collect())
    }
    async fn update_org_key(&self, k: OrgKey) -> StoreResult<()> {
        sqlx::query("UPDATE org_keys SET dek_wrapped=$2, kek_version=$3 WHERE org_id=$1")
            .bind(&k.org_id)
            .bind(&k.dek_wrapped)
            .bind(k.kek_version)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn insert_credential(&self, c: CredentialRow) -> StoreResult<CredentialRow> {
        sqlx::query(
            "INSERT INTO credentials (id, org_id, provider_id, name, kind, ciphertext, nonce, meta, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(&c.id)
        .bind(&c.org_id)
        .bind(&c.provider_id)
        .bind(&c.name)
        .bind(&c.kind)
        .bind(&c.ciphertext)
        .bind(&c.nonce)
        .bind(&c.meta)
        .bind(c.created_at)
        .execute(&self.pool)
        .await?;
        Ok(c)
    }
    async fn get_credential(&self, org_id: &str, id: &str) -> StoreResult<Option<CredentialRow>> {
        Ok(sqlx::query("SELECT * FROM credentials WHERE org_id=$1 AND id=$2")
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| credential(&r)))
    }
    async fn list_credentials(&self, org_id: &str) -> StoreResult<Vec<CredentialRow>> {
        Ok(sqlx::query("SELECT * FROM credentials WHERE org_id=$1 ORDER BY created_at")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(credential)
            .collect())
    }
    async fn find_credential(&self, org_id: &str, provider_id: &str) -> StoreResult<Option<CredentialRow>> {
        Ok(sqlx::query("SELECT * FROM credentials WHERE org_id=$1 AND provider_id=$2 ORDER BY created_at DESC, id DESC LIMIT 1")
            .bind(org_id)
            .bind(provider_id)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| credential(&r)))
    }
    async fn delete_credential(&self, org_id: &str, id: &str) -> StoreResult<bool> {
        Ok(sqlx::query("DELETE FROM credentials WHERE org_id=$1 AND id=$2").bind(org_id).bind(id).execute(&self.pool).await?.rows_affected() > 0)
    }
    async fn mark_credential(&self, id: &str, ok: bool, error: Option<String>) -> StoreResult<()> {
        if ok {
            sqlx::query("UPDATE credentials SET last_ok_at = now(), last_error = NULL WHERE id=$1").bind(id).execute(&self.pool).await?;
        } else {
            sqlx::query("UPDATE credentials SET last_error = $2 WHERE id=$1").bind(id).bind(error).execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn upsert_bundle(&self, b: BundleRow) -> StoreResult<BundleRow> {
        let r = sqlx::query(
            "INSERT INTO bundles (id, org_id, name, description, skill_md, tool_ids, credential_refs, version, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,1,$8)
             ON CONFLICT (org_id, id) DO UPDATE SET name=$3, description=$4, skill_md=$5, tool_ids=$6, credential_refs=$7,
               version = bundles.version + 1, updated_at=$8
             RETURNING *",
        )
        .bind(&b.id)
        .bind(&b.org_id)
        .bind(&b.name)
        .bind(&b.description)
        .bind(&b.skill_md)
        .bind(&b.tool_ids)
        .bind(&b.credential_refs)
        .bind(b.updated_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(bundle(&r))
    }
    async fn list_bundles(&self, org_id: &str) -> StoreResult<Vec<BundleRow>> {
        Ok(sqlx::query("SELECT * FROM bundles WHERE org_id=$1 ORDER BY id").bind(org_id).fetch_all(&self.pool).await?.iter().map(bundle).collect())
    }
    async fn get_bundle(&self, org_id: &str, id: &str) -> StoreResult<Option<BundleRow>> {
        Ok(sqlx::query("SELECT * FROM bundles WHERE org_id=$1 AND id=$2").bind(org_id).bind(id).fetch_optional(&self.pool).await?.map(|r| bundle(&r)))
    }

    async fn upsert_byo_tool(&self, t: ByoTool) -> StoreResult<ByoTool> {
        sqlx::query(
            "INSERT INTO byo_tools (id, org_id, base_url, credential_id, header, prefix, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (org_id, id) DO UPDATE SET base_url=$3, credential_id=$4, header=$5, prefix=$6",
        )
        .bind(&t.id)
        .bind(&t.org_id)
        .bind(&t.base_url)
        .bind(&t.credential_id)
        .bind(&t.header)
        .bind(&t.prefix)
        .bind(t.created_at)
        .execute(&self.pool)
        .await?;
        Ok(t)
    }
    async fn get_byo_tool(&self, org_id: &str, id: &str) -> StoreResult<Option<ByoTool>> {
        Ok(sqlx::query("SELECT * FROM byo_tools WHERE org_id=$1 AND id=$2").bind(org_id).bind(id).fetch_optional(&self.pool).await?.map(|r| byo(&r)))
    }
    async fn list_byo_tools(&self, org_id: Option<&str>) -> StoreResult<Vec<ByoTool>> {
        Ok(sqlx::query("SELECT * FROM byo_tools WHERE ($1::text IS NULL OR org_id = $1) ORDER BY id")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(byo)
            .collect())
    }

    async fn record_call(&self, c: NewCall, items: Vec<OutboxItem>, idem: Option<IdempotencyRow>) -> StoreResult<CallRow> {
        let mut tx = self.pool.begin().await?;
        // Serialise appends per project so the chain never forks.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))").bind(&c.project_id).execute(&mut *tx).await?;
        let last = sqlx::query("SELECT seq, row_hash FROM calls WHERE project_id=$1 ORDER BY seq DESC LIMIT 1")
            .bind(&c.project_id)
            .fetch_optional(&mut *tx)
            .await?;
        let (seq, prev_hash) = match last {
            Some(r) => (r.get::<i64, _>("seq") + 1, r.get::<String, _>("row_hash")),
            None => (1, GENESIS_HASH.to_string()),
        };
        let row_hash = chain_hash(&prev_hash, &c.hash_payload(seq));
        sqlx::query(
            "INSERT INTO calls (id, project_id, org_id, seq, key_public_id, target, tool_id, provider_id, side_effect, idempotency_key, request_hash,
               status, http_status, latency_ms, cost_micros, price_basis, mint_hold_id, mint_receipt, rago_event_id, cache, fallback_chain,
               created_at, prev_hash, row_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
        )
        .bind(&c.id)
        .bind(&c.project_id)
        .bind(&c.org_id)
        .bind(seq)
        .bind(&c.key_public_id)
        .bind(&c.target)
        .bind(&c.tool_id)
        .bind(&c.provider_id)
        .bind(&c.side_effect)
        .bind(&c.idempotency_key)
        .bind(&c.request_hash)
        .bind(&c.status)
        .bind(c.http_status)
        .bind(c.latency_ms)
        .bind(c.cost_micros)
        .bind(&c.price_basis)
        .bind(&c.mint_hold_id)
        .bind(&c.mint_receipt)
        .bind(&c.rago_event_id)
        .bind(&c.cache)
        .bind(&c.fallback_chain)
        .bind(c.created_at)
        .bind(&prev_hash)
        .bind(&row_hash)
        .execute(&mut *tx)
        .await?;
        for o in &items {
            sqlx::query("INSERT INTO outbox (id, kind, payload, attempts, next_attempt_at) VALUES ($1,$2,$3,0,$4)")
                .bind(&o.id)
                .bind(&o.kind)
                .bind(&o.payload)
                .bind(o.next_attempt_at)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(i) = &idem {
            sqlx::query(
                "INSERT INTO idempotency (project_id, key, request_hash, http_status, response, expires_at) VALUES ($1,$2,$3,$4,$5,$6)
                 ON CONFLICT (project_id, key) DO UPDATE SET request_hash=$3, http_status=$4, response=$5, expires_at=$6
                 WHERE idempotency.expires_at <= now()",
            )
            .bind(&i.project_id)
            .bind(&i.key)
            .bind(&i.request_hash)
            .bind(i.http_status)
            .bind(&i.response)
            .bind(i.expires_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(CallRow { call: c, seq, prev_hash, row_hash })
    }
    async fn get_call(&self, project_id: &str, id: &str) -> StoreResult<Option<CallRow>> {
        Ok(sqlx::query("SELECT * FROM calls WHERE project_id=$1 AND id=$2").bind(project_id).bind(id).fetch_optional(&self.pool).await?.map(|r| call(&r)))
    }
    async fn list_calls(&self, project_id: &str, before: Option<&str>, limit: i64) -> StoreResult<Vec<CallRow>> {
        Ok(sqlx::query(
            "SELECT * FROM calls WHERE project_id=$1
               AND seq < COALESCE((SELECT seq FROM calls WHERE project_id=$1 AND id=$2), 9223372036854775807)
             ORDER BY seq DESC LIMIT $3",
        )
        .bind(project_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(call)
        .collect())
    }
    async fn chain(&self, project_id: &str, upto_seq: i64) -> StoreResult<Vec<CallRow>> {
        Ok(sqlx::query("SELECT * FROM calls WHERE project_id=$1 AND seq <= $2 ORDER BY seq")
            .bind(project_id)
            .bind(upto_seq)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(call)
            .collect())
    }
    async fn spend_since(&self, f: SpendFilter) -> StoreResult<i64> {
        let agg = if f.count_calls { "COUNT(*) FILTER (WHERE status = 'ok' AND cache <> 'replay')" } else { "COALESCE(SUM(cost_micros), 0)" };
        let r = sqlx::query(&format!(
            "SELECT ({agg})::bigint AS v FROM calls WHERE project_id=$1
               AND ($2::timestamptz IS NULL OR created_at >= $2)
               AND ($3::text IS NULL OR key_public_id = $3)
               AND ($4::text[] IS NULL OR target = ANY($4) OR tool_id = ANY($4))
               AND ($5::text IS NULL OR side_effect = $5)"
        ))
        .bind(&f.project_id)
        .bind(f.since)
        .bind(&f.key_public_id)
        .bind(&f.targets)
        .bind(&f.side_effect)
        .fetch_one(&self.pool)
        .await?;
        Ok(r.get("v"))
    }
    async fn usage(&self, project_id: &str, from: DateTime<Utc>, to: DateTime<Utc>, group_by: GroupBy) -> StoreResult<Vec<UsageRow>> {
        let g = match group_by {
            GroupBy::Tool => "COALESCE(tool_id, target)",
            GroupBy::Provider => "COALESCE(provider_id, '-')",
            GroupBy::Key => "key_public_id",
            GroupBy::Day => "to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD')",
        };
        let rows = sqlx::query(&format!(
            "SELECT {g} AS grp, COUNT(*)::bigint AS calls, COUNT(*) FILTER (WHERE cost_micros > 0)::bigint AS billed,
                    COALESCE(SUM(cost_micros),0)::bigint AS cost
             FROM calls WHERE project_id=$1 AND created_at >= $2 AND created_at < $3 GROUP BY 1 ORDER BY 1"
        ))
        .bind(project_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| UsageRow { group: r.get("grp"), calls: r.get("calls"), billed_calls: r.get("billed"), cost_micros: r.get("cost") })
            .collect())
    }
    async fn get_idempotency(&self, project_id: &str, key: &str, now: DateTime<Utc>) -> StoreResult<Option<IdempotencyRow>> {
        let r = sqlx::query("SELECT * FROM idempotency WHERE project_id=$1 AND key=$2 AND expires_at > $3")
            .bind(project_id)
            .bind(key)
            .bind(now)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| IdempotencyRow {
            project_id: r.get("project_id"),
            key: r.get("key"),
            request_hash: r.get("request_hash"),
            http_status: r.get("http_status"),
            response: r.get("response"),
            expires_at: r.get("expires_at"),
        }))
    }
    async fn due_outbox(&self, now: DateTime<Utc>, limit: i64) -> StoreResult<Vec<OutboxItem>> {
        Ok(sqlx::query("SELECT * FROM outbox WHERE delivered_at IS NULL AND next_attempt_at <= $1 ORDER BY next_attempt_at LIMIT $2")
            .bind(now)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(outbox)
            .collect())
    }
    async fn mark_outbox(&self, id: &str, delivered: bool, error: Option<String>, next_attempt_at: DateTime<Utc>) -> StoreResult<()> {
        if delivered {
            sqlx::query("UPDATE outbox SET attempts = attempts + 1, delivered_at = now() WHERE id=$1").bind(id).execute(&self.pool).await?;
        } else {
            sqlx::query("UPDATE outbox SET attempts = attempts + 1, last_error=$2, next_attempt_at=$3 WHERE id=$1")
                .bind(id)
                .bind(error)
                .bind(next_attempt_at)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
    async fn record_stat(&self, tool_id: &str, success: bool, latency_ms: i64, cost_micros: i64) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO provider_stats (tool_id, calls, successes, total_latency_ms, observed_cost_micros) VALUES ($1, 1, $2, $3, $4)
             ON CONFLICT (tool_id) DO UPDATE SET calls = provider_stats.calls + 1, successes = provider_stats.successes + $2,
               total_latency_ms = provider_stats.total_latency_ms + $3, observed_cost_micros = provider_stats.observed_cost_micros + $4",
        )
        .bind(tool_id)
        .bind(success as i64)
        .bind(latency_ms)
        .bind(cost_micros)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn stats(&self) -> StoreResult<Vec<ToolStat>> {
        Ok(sqlx::query("SELECT * FROM provider_stats ORDER BY tool_id")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| ToolStat {
                tool_id: r.get("tool_id"),
                calls: r.get("calls"),
                successes: r.get("successes"),
                total_latency_ms: r.get("total_latency_ms"),
                observed_cost_micros: r.get("observed_cost_micros"),
            })
            .collect())
    }

    async fn insert_client(&self, c: OAuthClient) -> StoreResult<OAuthClient> {
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, metadata, redirect_uris, created_at) VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (client_id) DO UPDATE SET metadata=$3, redirect_uris=$4",
        )
        .bind(&c.client_id)
        .bind(&c.kind)
        .bind(&c.metadata)
        .bind(&c.redirect_uris)
        .bind(c.created_at)
        .execute(&self.pool)
        .await?;
        Ok(c)
    }
    async fn get_client(&self, client_id: &str) -> StoreResult<Option<OAuthClient>> {
        Ok(sqlx::query("SELECT * FROM oauth_clients WHERE client_id=$1").bind(client_id).fetch_optional(&self.pool).await?.map(|r| client(&r)))
    }
    async fn insert_code(&self, c: OAuthCode) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO oauth_codes (code_hash, client_id, user_id, org_id, project_id, scopes, redirect_uri, code_challenge, resource, expires_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(&c.code_hash)
        .bind(&c.client_id)
        .bind(&c.user_id)
        .bind(&c.org_id)
        .bind(&c.project_id)
        .bind(&c.scopes)
        .bind(&c.redirect_uri)
        .bind(&c.code_challenge)
        .bind(&c.resource)
        .bind(c.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn take_code(&self, code_hash: &str) -> StoreResult<Option<OAuthCode>> {
        let r = sqlx::query("DELETE FROM oauth_codes WHERE code_hash=$1 RETURNING *").bind(code_hash).fetch_optional(&self.pool).await?;
        Ok(r.map(|r| OAuthCode {
            code_hash: r.get("code_hash"),
            client_id: r.get("client_id"),
            user_id: r.get("user_id"),
            org_id: r.get("org_id"),
            project_id: r.get("project_id"),
            scopes: r.get("scopes"),
            redirect_uri: r.get("redirect_uri"),
            code_challenge: r.get("code_challenge"),
            resource: r.get("resource"),
            expires_at: r.get("expires_at"),
        }))
    }
    async fn insert_grant(&self, g: OAuthGrant) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO oauth_grants (id, family_id, client_id, user_id, org_id, project_id, scopes, refresh_hash, resource, expires_at, used_at, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(&g.id)
        .bind(&g.family_id)
        .bind(&g.client_id)
        .bind(&g.user_id)
        .bind(&g.org_id)
        .bind(&g.project_id)
        .bind(&g.scopes)
        .bind(&g.refresh_hash)
        .bind(&g.resource)
        .bind(g.expires_at)
        .bind(g.used_at)
        .bind(g.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn find_grant_by_refresh(&self, refresh_hash: &str) -> StoreResult<Option<OAuthGrant>> {
        Ok(sqlx::query("SELECT * FROM oauth_grants WHERE refresh_hash=$1").bind(refresh_hash).fetch_optional(&self.pool).await?.map(|r| grant(&r)))
    }
    async fn consume_grant(&self, id: &str) -> StoreResult<bool> {
        Ok(sqlx::query("UPDATE oauth_grants SET used_at = now() WHERE id=$1 AND used_at IS NULL").bind(id).execute(&self.pool).await?.rows_affected() == 1)
    }
    async fn revoke_family(&self, family_id: &str) -> StoreResult<()> {
        sqlx::query("INSERT INTO oauth_revoked_families (family_id) VALUES ($1) ON CONFLICT DO NOTHING").bind(family_id).execute(&self.pool).await?;
        Ok(())
    }
    async fn family_revoked(&self, family_id: &str) -> StoreResult<bool> {
        Ok(sqlx::query("SELECT 1 FROM oauth_revoked_families WHERE family_id=$1").bind(family_id).fetch_optional(&self.pool).await?.is_some())
    }
    async fn add_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<()> {
        sqlx::query("INSERT INTO oauth_consents (family_id, tool_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
            .bind(family_id)
            .bind(tool_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn has_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<bool> {
        Ok(sqlx::query("SELECT 1 FROM oauth_consents WHERE family_id=$1 AND tool_id=$2")
            .bind(family_id)
            .bind(tool_id)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
    }
    async fn insert_device(&self, d: DeviceCode) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO device_codes (device_hash, user_code, client_id, scopes, status, user_id, org_id, project_id, expires_at, last_poll_at, interval_s)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(&d.device_hash)
        .bind(&d.user_code)
        .bind(&d.client_id)
        .bind(&d.scopes)
        .bind(&d.status)
        .bind(&d.user_id)
        .bind(&d.org_id)
        .bind(&d.project_id)
        .bind(d.expires_at)
        .bind(d.last_poll_at)
        .bind(d.interval_s)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn get_device_by_user_code(&self, user_code: &str) -> StoreResult<Option<DeviceCode>> {
        Ok(sqlx::query("SELECT * FROM device_codes WHERE user_code=$1").bind(user_code).fetch_optional(&self.pool).await?.map(|r| device(&r)))
    }
    async fn get_device(&self, device_hash: &str) -> StoreResult<Option<DeviceCode>> {
        Ok(sqlx::query("SELECT * FROM device_codes WHERE device_hash=$1").bind(device_hash).fetch_optional(&self.pool).await?.map(|r| device(&r)))
    }
    async fn update_device(&self, d: DeviceCode) -> StoreResult<()> {
        sqlx::query("UPDATE device_codes SET status=$2, user_id=$3, org_id=$4, project_id=$5, last_poll_at=$6 WHERE device_hash=$1")
            .bind(&d.device_hash)
            .bind(&d.status)
            .bind(&d.user_id)
            .bind(&d.org_id)
            .bind(&d.project_id)
            .bind(d.last_poll_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

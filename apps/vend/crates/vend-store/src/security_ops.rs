//! Durable, replica-safe security operations.
//!
//! This module intentionally accepts only digests for attacker-controlled
//! subjects. Passwords, bearer tokens, OAuth state, provisioning keys and raw
//! network addresses must never be persisted in either rate-limit or audit
//! storage.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use ulid::Ulid;
use vend_core::error::VendError;
use vend_core::ids::TenantId;

use crate::{
    hash_password, mint_bearer, password_matches, row_to_customer_user, store_err,
    validate_password, validate_ttl, CustomerPrincipal, IssuedCustomerSession, VendStore,
    CUSTOMER_USER_SELECT,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    Administrator,
    Customer,
    ProvisioningKey,
    System,
}

impl AuditActorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Administrator => "administrator",
            Self::Customer => "customer",
            Self::ProvisioningKey => "provisioning_key",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Succeeded,
    Denied,
    Failed,
}

impl AuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// A deliberately metadata-only event. Callers must use stable identifiers;
/// request/response bodies and credentials are rejected defensively.
#[derive(Clone, Debug)]
pub struct NewAuditEvent {
    pub tenant_id: Option<TenantId>,
    pub actor_kind: AuditActorKind,
    pub actor_id: Option<String>,
    pub action: String,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub outcome: AuditOutcome,
    pub request_id: Option<String>,
    pub source_hash: Option<[u8; 32]>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuditEvent {
    pub id: String,
    pub occurred_at: DateTime<Utc>,
    pub tenant_id: Option<TenantId>,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub action: String,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub outcome: String,
    pub request_id: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug)]
pub struct RateLimitRequest {
    pub bucket: String,
    /// A tenant id or the literal `global`; never an email or bearer.
    pub tenant_scope: String,
    /// SHA-256/HMAC-like digest produced with the runtime rate-limit pepper.
    pub subject_hash: [u8; 32],
    pub window_seconds: i32,
    pub max_attempts: i32,
    pub block_seconds: i32,
    /// Maximum expired rows removed by this request. This bounds cleanup work.
    pub cleanup_limit: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub remaining: u32,
    pub retry_after_seconds: u64,
}

#[async_trait]
pub trait SecurityOpsRepo: Send + Sync {
    async fn append_audit_event(&self, event: NewAuditEvent) -> Result<AuditEvent, VendError>;
    async fn check_rate_limit(
        &self,
        request: RateLimitRequest,
    ) -> Result<RateLimitDecision, VendError>;
    async fn clear_rate_limit(
        &self,
        bucket: &str,
        tenant_scope: &str,
        subject_hash: [u8; 32],
    ) -> Result<(), VendError>;
    async fn rotate_customer_session(
        &self,
        principal: &CustomerPrincipal,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError>;
    async fn change_customer_password(
        &self,
        principal: &CustomerPrincipal,
        current_password: &str,
        new_password: &str,
    ) -> Result<u64, VendError>;
}

#[async_trait]
impl SecurityOpsRepo for VendStore {
    async fn append_audit_event(&self, event: NewAuditEvent) -> Result<AuditEvent, VendError> {
        validate_audit_event(&event)?;
        let id = format!("vaud_{}", Ulid::new());
        let row = sqlx::query(
            "INSERT INTO vend_audit_events \
             (id, tenant_id, actor_kind, actor_id, action, target_kind, target_id, outcome, \
              request_id, source_hash, metadata) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
             RETURNING id, occurred_at, tenant_id, actor_kind, actor_id, action, target_kind, \
                       target_id, outcome, request_id, metadata",
        )
        .bind(&id)
        .bind(event.tenant_id.as_ref().map(TenantId::as_str))
        .bind(event.actor_kind.as_str())
        .bind(event.actor_id)
        .bind(event.action)
        .bind(event.target_kind)
        .bind(event.target_id)
        .bind(event.outcome.as_str())
        .bind(event.request_id)
        .bind(event.source_hash.map(|digest| digest.to_vec()))
        .bind(event.metadata)
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        row_to_audit_event(&row)
    }

    async fn check_rate_limit(
        &self,
        request: RateLimitRequest,
    ) -> Result<RateLimitDecision, VendError> {
        validate_rate_limit(&request)?;

        // Cleanup is deliberately bounded and uses an indexable timestamp. It
        // is best-effort housekeeping, never an unbounded request-path sweep.
        sqlx::query(
            "DELETE FROM vend_rate_limits WHERE ctid IN ( \
               SELECT ctid FROM vend_rate_limits \
               WHERE updated_at < now() - interval '1 day' \
               ORDER BY updated_at LIMIT $1 \
             )",
        )
        .bind(request.cleanup_limit)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;

        let row = sqlx::query(
            "INSERT INTO vend_rate_limits \
               (bucket, tenant_scope, subject_hash, window_started_at, attempts, blocked_until) \
             VALUES ($1,$2,$3,now(),1,NULL) \
             ON CONFLICT (bucket, tenant_scope, subject_hash) DO UPDATE SET \
               attempts = CASE \
                 WHEN vend_rate_limits.blocked_until > now() THEN vend_rate_limits.attempts \
                 WHEN vend_rate_limits.window_started_at <= now() - make_interval(secs => $4) THEN 1 \
                 ELSE vend_rate_limits.attempts + 1 END, \
               window_started_at = CASE \
                 WHEN vend_rate_limits.blocked_until > now() THEN vend_rate_limits.window_started_at \
                 WHEN vend_rate_limits.window_started_at <= now() - make_interval(secs => $4) THEN now() \
                 ELSE vend_rate_limits.window_started_at END, \
               blocked_until = CASE \
                 WHEN vend_rate_limits.blocked_until > now() THEN vend_rate_limits.blocked_until \
                 WHEN vend_rate_limits.window_started_at <= now() - make_interval(secs => $4) THEN NULL \
                 WHEN vend_rate_limits.attempts >= $5 THEN now() + make_interval(secs => $6) \
                 ELSE NULL END, \
               updated_at = now() \
             RETURNING attempts, blocked_until, now() AS checked_at",
        )
        .bind(&request.bucket)
        .bind(&request.tenant_scope)
        .bind(request.subject_hash.as_slice())
        .bind(request.window_seconds)
        .bind(request.max_attempts)
        .bind(request.block_seconds)
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        let attempts: i32 = row.try_get("attempts").map_err(store_err)?;
        let blocked_until: Option<DateTime<Utc>> =
            row.try_get("blocked_until").map_err(store_err)?;
        let checked_at: DateTime<Utc> = row.try_get("checked_at").map_err(store_err)?;
        let retry_after_seconds = blocked_until
            .map(|until| {
                let millis = (until - checked_at).num_milliseconds().max(0) as u64;
                millis.div_ceil(1_000).max(1)
            })
            .unwrap_or(0);
        Ok(RateLimitDecision {
            allowed: blocked_until.is_none(),
            remaining: request.max_attempts.saturating_sub(attempts).max(0) as u32,
            retry_after_seconds,
        })
    }

    async fn clear_rate_limit(
        &self,
        bucket: &str,
        tenant_scope: &str,
        subject_hash: [u8; 32],
    ) -> Result<(), VendError> {
        validate_dimension(bucket, 64)?;
        validate_dimension(tenant_scope, 128)?;
        sqlx::query(
            "DELETE FROM vend_rate_limits \
             WHERE bucket = $1 AND tenant_scope = $2 AND subject_hash = $3",
        )
        .bind(bucket)
        .bind(tenant_scope)
        .bind(subject_hash.as_slice())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn rotate_customer_session(
        &self,
        principal: &CustomerPrincipal,
        session_ttl_seconds: i64,
    ) -> Result<IssuedCustomerSession, VendError> {
        let ttl = validate_ttl(session_ttl_seconds, 30 * 24 * 60 * 60)?;
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        let live = sqlx::query(
            "SELECT s.id FROM tenant_sessions s \
             JOIN tenant_memberships m ON m.id = s.membership_id \
             JOIN customer_users u ON u.id = m.user_id \
             WHERE s.id = $1 AND s.membership_id = $2 AND m.user_id = $3 AND m.tenant_id = $4 \
               AND s.revoked_at IS NULL AND s.expires_at > now() \
               AND m.status = 'active' AND u.status = 'active' FOR UPDATE OF s",
        )
        .bind(&principal.session_id)
        .bind(&principal.membership_id)
        .bind(&principal.user_id)
        .bind(principal.tenant_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?;
        if live.is_none() {
            return Err(VendError::Unauthorized);
        }
        let query = format!("{CUSTOMER_USER_SELECT} WHERE m.id = $1");
        let user = sqlx::query(&query)
            .bind(&principal.membership_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_err)
            .and_then(|row| row_to_customer_user(&row))?;
        let bearer = mint_bearer("vses", "vss");
        let expires_at = Utc::now() + ttl;
        sqlx::query(
            "INSERT INTO tenant_sessions (id, membership_id, prefix, secret_hash, expires_at) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&bearer.id)
        .bind(&principal.membership_id)
        .bind(&bearer.prefix)
        .bind(bearer.digest.as_slice())
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        let revoked = sqlx::query(
            "UPDATE tenant_sessions SET revoked_at = now() \
             WHERE id = $1 AND membership_id = $2 AND revoked_at IS NULL",
        )
        .bind(&principal.session_id)
        .bind(&principal.membership_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        if revoked.rows_affected() != 1 {
            return Err(VendError::Unauthorized);
        }
        append_customer_audit_tx(
            &mut tx,
            principal,
            "auth.session_rotated",
            "session",
            &bearer.id,
            serde_json::json!({"previous_session_id": principal.session_id}),
        )
        .await?;
        tx.commit().await.map_err(store_err)?;
        Ok(IssuedCustomerSession {
            user,
            session_id: bearer.id,
            session_token: bearer.plaintext,
            expires_at,
        })
    }

    async fn change_customer_password(
        &self,
        principal: &CustomerPrincipal,
        current_password: &str,
        new_password: &str,
    ) -> Result<u64, VendError> {
        validate_password(current_password).map_err(|_| VendError::Unauthorized)?;
        validate_password(new_password)?;
        let mut tx = self.pool.begin().await.map_err(store_err)?;
        let row = sqlx::query(
            "SELECT u.password_hash FROM customer_users u \
             JOIN tenant_memberships m ON m.user_id = u.id \
             JOIN tenant_sessions s ON s.membership_id = m.id \
             WHERE u.id = $1 AND m.id = $2 AND m.tenant_id = $3 AND s.id = $4 \
               AND u.status = 'active' AND m.status = 'active' \
               AND s.revoked_at IS NULL AND s.expires_at > now() FOR UPDATE OF u, s",
        )
        .bind(&principal.user_id)
        .bind(&principal.membership_id)
        .bind(principal.tenant_id.as_str())
        .bind(&principal.session_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or(VendError::Unauthorized)?;
        let old_hash: Option<String> = row.try_get("password_hash").map_err(store_err)?;
        let Some(old_hash) = old_hash else {
            return Err(VendError::Unauthorized);
        };
        if !password_matches(current_password.to_string(), old_hash).await? {
            return Err(VendError::Unauthorized);
        }
        let new_hash = hash_password(new_password.to_string()).await?;
        sqlx::query("UPDATE customer_users SET password_hash = $2 WHERE id = $1")
            .bind(&principal.user_id)
            .bind(new_hash)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        let revoked = sqlx::query(
            "UPDATE tenant_sessions SET revoked_at = now() \
             WHERE membership_id IN (SELECT id FROM tenant_memberships WHERE user_id = $1) \
               AND id <> $2 AND revoked_at IS NULL",
        )
        .bind(&principal.user_id)
        .bind(&principal.session_id)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?
        .rows_affected();
        append_customer_audit_tx(
            &mut tx,
            principal,
            "auth.password_changed",
            "customer_user",
            &principal.user_id,
            serde_json::json!({"other_sessions_revoked": revoked}),
        )
        .await?;
        tx.commit().await.map_err(store_err)?;
        Ok(revoked)
    }
}

async fn append_customer_audit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &CustomerPrincipal,
    action: &str,
    target_kind: &str,
    target_id: &str,
    metadata: Value,
) -> Result<(), VendError> {
    sqlx::query(
        "INSERT INTO vend_audit_events \
         (id, tenant_id, actor_kind, actor_id, action, target_kind, target_id, outcome, metadata) \
         VALUES ($1,$2,'customer',$3,$4,$5,$6,'succeeded',$7)",
    )
    .bind(format!("vaud_{}", Ulid::new()))
    .bind(principal.tenant_id.as_str())
    .bind(&principal.user_id)
    .bind(action)
    .bind(target_kind)
    .bind(target_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map_err(store_err)?;
    Ok(())
}

fn validate_rate_limit(request: &RateLimitRequest) -> Result<(), VendError> {
    validate_dimension(&request.bucket, 64)?;
    validate_dimension(&request.tenant_scope, 128)?;
    if !(1..=86_400).contains(&request.window_seconds)
        || !(1..=10_000).contains(&request.max_attempts)
        || !(1..=86_400).contains(&request.block_seconds)
        || !(1..=1_000).contains(&request.cleanup_limit)
    {
        return Err(VendError::Config("invalid rate-limit policy".into()));
    }
    Ok(())
}

fn validate_dimension(value: &str, maximum: usize) -> Result<(), VendError> {
    if value.is_empty()
        || value.len() > maximum
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(VendError::Config("invalid security dimension".into()));
    }
    Ok(())
}

fn validate_audit_event(event: &NewAuditEvent) -> Result<(), VendError> {
    validate_dimension(&event.action, 128)?;
    if event.action.len() < 3 {
        return Err(VendError::Config("audit action is too short".into()));
    }
    for value in [
        event.actor_id.as_deref(),
        event.target_kind.as_deref(),
        event.target_id.as_deref(),
        event.request_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_dimension(value, 255)?;
    }
    if !event.metadata.is_object()
        || serde_json::to_vec(&event.metadata)
            .map_err(|error| VendError::Storage(format!("audit metadata: {error}")))?
            .len()
            > 4_096
        || contains_sensitive_metadata(&event.metadata)
    {
        return Err(VendError::Config(
            "audit metadata must be a small secret-free object".into(),
        ));
    }
    Ok(())
}

fn contains_sensitive_metadata(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase();
            [
                "password",
                "secret",
                "token",
                "authorization",
                "credential",
                "request_body",
                "response_body",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
                || contains_sensitive_metadata(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_metadata),
        _ => false,
    }
}

fn row_to_audit_event(row: &sqlx::postgres::PgRow) -> Result<AuditEvent, VendError> {
    let tenant_id: Option<String> = row.try_get("tenant_id").map_err(store_err)?;
    Ok(AuditEvent {
        id: row.try_get("id").map_err(store_err)?,
        occurred_at: row.try_get("occurred_at").map_err(store_err)?,
        tenant_id: tenant_id.map(|value| TenantId::parse(&value)).transpose()?,
        actor_kind: row.try_get("actor_kind").map_err(store_err)?,
        actor_id: row.try_get("actor_id").map_err(store_err)?,
        action: row.try_get("action").map_err(store_err)?,
        target_kind: row.try_get("target_kind").map_err(store_err)?,
        target_id: row.try_get("target_id").map_err(store_err)?,
        outcome: row.try_get("outcome").map_err(store_err)?,
        request_id: row.try_get("request_id").map_err(store_err)?,
        metadata: row.try_get("metadata").map_err(store_err)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(metadata: Value) -> NewAuditEvent {
        NewAuditEvent {
            tenant_id: None,
            actor_kind: AuditActorKind::System,
            actor_id: None,
            action: "reconciliation.hold_released".into(),
            target_kind: Some("charge".into()),
            target_id: Some("chg_01".into()),
            outcome: AuditOutcome::Succeeded,
            request_id: None,
            source_hash: None,
            metadata,
        }
    }

    #[test]
    fn audit_metadata_rejects_secrets_and_bodies_recursively() {
        assert!(validate_audit_event(&audit(serde_json::json!({
            "provider": "github",
            "amount_micros": 42
        })))
        .is_ok());
        for metadata in [
            serde_json::json!({"password":"x"}),
            serde_json::json!({"nested":{"access_token":"x"}}),
            serde_json::json!({"request_body":{"email":"a@example.test"}}),
        ] {
            assert!(validate_audit_event(&audit(metadata)).is_err());
        }
    }

    #[test]
    fn rate_policy_and_dimensions_are_bounded() {
        let valid = RateLimitRequest {
            bucket: "native_login".into(),
            tenant_scope: "ten_01".into(),
            subject_hash: [7; 32],
            window_seconds: 60,
            max_attempts: 5,
            block_seconds: 300,
            cleanup_limit: 100,
        };
        assert!(validate_rate_limit(&valid).is_ok());
        assert!(validate_rate_limit(&RateLimitRequest {
            max_attempts: 0,
            ..valid.clone()
        })
        .is_err());
        assert!(validate_rate_limit(&RateLimitRequest {
            cleanup_limit: 1_001,
            ..valid
        })
        .is_err());
    }
}

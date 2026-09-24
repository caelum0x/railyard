//! Customer portal sessions: one row per issued portal link (revocable), plus
//! the customer-scoped reads the portal pages need.

use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A stored portal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalSession {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Db {
    /// Record a newly issued portal session.
    pub async fn insert_portal_session(
        &self,
        id: Uuid,
        organization_id: Uuid,
        customer_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO customer_portal_sessions (id, organization_id, customer_id, expires_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(organization_id)
        .bind(customer_id)
        .bind(expires_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A portal session by id (and mark it used).
    pub async fn touch_portal_session(&self, id: Uuid) -> Result<Option<PortalSession>> {
        let row = sqlx::query(
            "UPDATE customer_portal_sessions SET last_used_at = (now() AT TIME ZONE 'UTC') WHERE id = $1 \
             RETURNING id, organization_id, customer_id, expires_at, revoked_at",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<PortalSession> {
            let expires: NaiveDateTime = r.try_get("expires_at")?;
            let revoked: Option<NaiveDateTime> = r.try_get("revoked_at")?;
            Ok(PortalSession {
                id: r.try_get("id")?,
                organization_id: r.try_get("organization_id")?,
                customer_id: r.try_get("customer_id")?,
                expires_at: expires.and_utc(),
                revoked_at: revoked.map(|t| t.and_utc()),
            })
        })
        .transpose()
    }

    /// Revoke every open portal session of a customer; returns how many.
    pub async fn revoke_portal_sessions(&self, organization_id: Uuid, customer_id: Uuid) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE customer_portal_sessions SET revoked_at = (now() AT TIME ZONE 'UTC') \
             WHERE organization_id = $1 AND customer_id = $2 AND revoked_at IS NULL \
               AND expires_at > (now() AT TIME ZONE 'UTC')",
        )
        .bind(organization_id)
        .bind(customer_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Revoke one portal session of a customer.
    pub async fn revoke_portal_session(&self, organization_id: Uuid, session_id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE customer_portal_sessions SET revoked_at = (now() AT TIME ZONE 'UTC') \
             WHERE id = $1 AND organization_id = $2 AND revoked_at IS NULL",
        )
        .bind(session_id)
        .bind(organization_id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The organization's display name.
    pub async fn organization_name(&self, organization_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query("SELECT name FROM organizations WHERE id = $1")
            .bind(organization_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.try_get::<Option<String>, _>("name").ok().flatten()))
    }
}

//! Postgres implementation of Relay's principal-bound AITP challenge store.

use async_trait::async_trait;
use relay_aitp::{AitpError, ChallengeRecord, ChallengeState, ChallengeStore, Result};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PostgresChallengeStore {
    pool: PgPool,
}

impl PostgresChallengeStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn locked(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        request_id: Uuid,
    ) -> Result<ChallengeRecord> {
        let row = sqlx::query(
            "SELECT request_id,owner_principal,challenge,state,operation_hash,receipt_hash, \
             issued_at_ms,updated_at_ms FROM relay_aitp_challenges \
             WHERE request_id=$1 FOR UPDATE",
        )
        .bind(request_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db_error)?
        .ok_or(AitpError::NotFound)?;
        decode(&row)
    }
}

#[async_trait]
impl ChallengeStore for PostgresChallengeStore {
    async fn issue(&self, record: ChallengeRecord) -> Result<()> {
        let challenge = serde_json::to_value(&record.challenge)
            .map_err(|error| AitpError::Store(error.to_string()))?;
        let result = sqlx::query(
            "INSERT INTO relay_aitp_challenges \
             (request_id,owner_principal,challenge,state,issued_at_ms,updated_at_ms,expires_at) \
             VALUES ($1,$2,$3,'issued',$4,$5,to_timestamp($6::double precision / 1000.0)) \
             ON CONFLICT (request_id) DO UPDATE SET challenge=EXCLUDED.challenge \
             WHERE relay_aitp_challenges.owner_principal=EXCLUDED.owner_principal \
             AND relay_aitp_challenges.challenge=EXCLUDED.challenge \
             AND relay_aitp_challenges.issued_at_ms=EXCLUDED.issued_at_ms",
        )
        .bind(record.request_id)
        .bind(&record.owner_principal)
        .bind(challenge)
        .bind(record.issued_at_ms)
        .bind(record.updated_at_ms)
        .bind(record.challenge.expiry as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if result.rows_affected() != 1 {
            return Err(AitpError::ConflictingReplay);
        }
        Ok(())
    }

    async fn get(&self, request_id: Uuid) -> Result<Option<ChallengeRecord>> {
        let row = sqlx::query(
            "SELECT request_id,owner_principal,challenge,state,operation_hash,receipt_hash, \
             issued_at_ms,updated_at_ms FROM relay_aitp_challenges WHERE request_id=$1",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        row.as_ref().map(decode).transpose()
    }

    async fn authorize(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord> {
        let mut transaction = self.pool.begin().await.map_err(db_error)?;
        let mut record = Self::locked(&mut transaction, request_id).await?;
        ensure_owner(&record, owner_principal)?;
        if i64::try_from(record.challenge.expiry).unwrap_or(i64::MAX) <= now_ms {
            sqlx::query(
                "UPDATE relay_aitp_challenges SET state='expired',updated_at_ms=$2 \
                 WHERE request_id=$1 AND state IN ('issued','authorized')",
            )
            .bind(request_id)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
            transaction.commit().await.map_err(db_error)?;
            return Err(AitpError::Expired);
        }
        match &record.state {
            ChallengeState::Issued => {
                sqlx::query(
                    "UPDATE relay_aitp_challenges SET state='authorized',operation_hash=$2,updated_at_ms=$3 \
                     WHERE request_id=$1 AND state='issued'",
                )
                .bind(request_id)
                .bind(operation_hash)
                .bind(now_ms)
                .execute(&mut *transaction)
                .await
                .map_err(db_error)?;
                record.state = ChallengeState::Authorized {
                    operation_hash: operation_hash.into(),
                };
                record.updated_at_ms = now_ms;
            }
            ChallengeState::Authorized {
                operation_hash: existing,
            } if existing == operation_hash => {}
            ChallengeState::Authorized { .. } => return Err(AitpError::ConflictingReplay),
            ChallengeState::Consumed { .. } => return Err(AitpError::AlreadyConsumed),
            ChallengeState::Expired => return Err(AitpError::Expired),
        }
        transaction.commit().await.map_err(db_error)?;
        Ok(record)
    }

    async fn consume(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        receipt_hash: &str,
        now_ms: i64,
    ) -> Result<ChallengeRecord> {
        let mut transaction = self.pool.begin().await.map_err(db_error)?;
        let mut record = Self::locked(&mut transaction, request_id).await?;
        ensure_owner(&record, owner_principal)?;
        match &record.state {
            ChallengeState::Authorized {
                operation_hash: existing,
            } if existing == operation_hash => {
                sqlx::query(
                    "UPDATE relay_aitp_challenges SET state='consumed',receipt_hash=$2,updated_at_ms=$3 \
                     WHERE request_id=$1 AND state='authorized' AND operation_hash=$4",
                )
                .bind(request_id)
                .bind(receipt_hash)
                .bind(now_ms)
                .bind(operation_hash)
                .execute(&mut *transaction)
                .await
                .map_err(db_error)?;
                record.state = ChallengeState::Consumed {
                    receipt_hash: receipt_hash.into(),
                };
                record.updated_at_ms = now_ms;
            }
            ChallengeState::Consumed {
                receipt_hash: existing,
            } if existing == receipt_hash => {}
            ChallengeState::Issued | ChallengeState::Authorized { .. } => {
                return Err(AitpError::AuthorizationMismatch);
            }
            ChallengeState::Consumed { .. } => return Err(AitpError::ConflictingReplay),
            ChallengeState::Expired => return Err(AitpError::Expired),
        }
        transaction.commit().await.map_err(db_error)?;
        Ok(record)
    }

    async fn purge_before(&self, cutoff_ms: i64, limit: usize) -> Result<u64> {
        let limit = i64::try_from(limit).map_err(|_| AitpError::InvalidRetentionLimit)?;
        if limit <= 0 {
            return Err(AitpError::InvalidRetentionLimit);
        }
        let result = sqlx::query(
            "WITH doomed AS ( \
                 SELECT request_id FROM relay_aitp_challenges \
                 WHERE ((state IN ('issued','authorized') AND expires_at <= to_timestamp($1::double precision / 1000.0)) \
                    OR (state IN ('consumed','expired') AND updated_at_ms <= $1)) \
                 ORDER BY updated_at_ms, request_id LIMIT $2 FOR UPDATE SKIP LOCKED \
             ) \
             DELETE FROM relay_aitp_challenges AS challenge USING doomed \
             WHERE challenge.request_id=doomed.request_id",
        )
        .bind(cutoff_ms)
        .bind(limit)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(result.rows_affected())
    }
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<ChallengeRecord> {
    let challenge = serde_json::from_value(row.try_get("challenge").map_err(db_error)?)
        .map_err(|error| AitpError::Store(error.to_string()))?;
    let state = match row
        .try_get::<String, _>("state")
        .map_err(db_error)?
        .as_str()
    {
        "issued" => ChallengeState::Issued,
        "authorized" => ChallengeState::Authorized {
            operation_hash: row
                .try_get::<Option<String>, _>("operation_hash")
                .map_err(db_error)?
                .ok_or_else(|| {
                    AitpError::Store("authorized challenge has no operation hash".into())
                })?,
        },
        "consumed" => ChallengeState::Consumed {
            receipt_hash: row
                .try_get::<Option<String>, _>("receipt_hash")
                .map_err(db_error)?
                .ok_or_else(|| AitpError::Store("consumed challenge has no receipt hash".into()))?,
        },
        "expired" => ChallengeState::Expired,
        other => {
            return Err(AitpError::Store(format!(
                "unknown challenge state '{other}'"
            )));
        }
    };
    Ok(ChallengeRecord {
        request_id: row.try_get("request_id").map_err(db_error)?,
        owner_principal: row.try_get("owner_principal").map_err(db_error)?,
        challenge,
        state,
        issued_at_ms: row.try_get("issued_at_ms").map_err(db_error)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(db_error)?,
    })
}

fn ensure_owner(record: &ChallengeRecord, owner_principal: &str) -> Result<()> {
    if record.owner_principal != owner_principal {
        return Err(AitpError::Ownership);
    }
    Ok(())
}

fn db_error(error: impl std::fmt::Display) -> AitpError {
    AitpError::Store(error.to_string())
}

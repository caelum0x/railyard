//! Postgres implementation of the durable settlement command journal.

use aitp_client::{AmountMicros, HoldRef, PaymentAuthorization, Receipt, Scheme};
use async_trait::async_trait;
use sqlx::postgres::PgPool;

use crate::error::TurnstileError;
use crate::journal::{SettlementJournal, SettlementPhase, SettlementRecord};

const COLUMNS: &str = "nonce, authorization_json, phase, hold_id, scheme_json, actual_micros, receipt_json, owner_token, lease_until_ms, last_error";
const RETURNING_COLUMNS: &str = "settlement.nonce, settlement.authorization_json, settlement.phase, settlement.hold_id, settlement.scheme_json, settlement.actual_micros, settlement.receipt_json, settlement.owner_token, settlement.lease_until_ms, settlement.last_error";

#[derive(sqlx::FromRow)]
struct StoredSettlement {
    nonce: String,
    authorization_json: String,
    phase: String,
    hold_id: Option<String>,
    scheme_json: Option<String>,
    actual_micros: Option<i64>,
    receipt_json: Option<String>,
    owner_token: String,
    lease_until_ms: i64,
    last_error: String,
}

impl TryFrom<StoredSettlement> for SettlementRecord {
    type Error = TurnstileError;

    fn try_from(row: StoredSettlement) -> Result<Self, Self::Error> {
        let authorization: PaymentAuthorization = decode(&row.authorization_json, "authorization")?;
        if authorization.nonce != row.nonce {
            return Err(storage("stored authorization nonce does not match row key"));
        }
        Ok(Self {
            authorization,
            phase: decode_phase(&row.phase)?,
            hold: row.hold_id.map(HoldRef),
            scheme: row
                .scheme_json
                .as_deref()
                .map(|value| decode(value, "scheme"))
                .transpose()?,
            actual_micros: row.actual_micros.map(AmountMicros),
            receipt: row
                .receipt_json
                .as_deref()
                .map(|value| decode(value, "receipt"))
                .transpose()?,
            owner_token: row.owner_token,
            lease_until_ms: row.lease_until_ms,
            last_error: row.last_error,
        })
    }
}

fn storage(message: impl Into<String>) -> TurnstileError {
    TurnstileError::Storage(message.into())
}

fn db(error: sqlx::Error) -> TurnstileError {
    storage(error.to_string())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, TurnstileError> {
    serde_json::to_string(value).map_err(|error| storage(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str, field: &str) -> Result<T, TurnstileError> {
    serde_json::from_str(value)
        .map_err(|error| storage(format!("invalid stored settlement {field}: {error}")))
}

fn decode_phase(value: &str) -> Result<SettlementPhase, TurnstileError> {
    match value {
        "verifying" => Ok(SettlementPhase::Verifying),
        "held" => Ok(SettlementPhase::Held),
        "capture_pending" => Ok(SettlementPhase::CapturePending),
        "captured" => Ok(SettlementPhase::Captured),
        "release_pending" => Ok(SettlementPhase::ReleasePending),
        "released" => Ok(SettlementPhase::Released),
        "transfer_complete" => Ok(SettlementPhase::TransferComplete),
        "rejected" => Ok(SettlementPhase::Rejected),
        other => Err(storage(format!("invalid settlement phase {other:?}"))),
    }
}

/// Shared, crash-recoverable settlement journal. Cheap to clone.
#[derive(Clone)]
pub struct PostgresSettlementJournal {
    pool: PgPool,
}

impl PostgresSettlementJournal {
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn fetch(&self, nonce: &str) -> Result<Option<SettlementRecord>, TurnstileError> {
        let query = format!("SELECT {COLUMNS} FROM turnstile_settlements WHERE nonce = $1");
        let row = sqlx::query_as::<_, StoredSettlement>(&query)
            .bind(nonce)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        row.map(TryInto::try_into).transpose()
    }

    async fn required(&self, nonce: &str) -> Result<SettlementRecord, TurnstileError> {
        self.fetch(nonce)
            .await?
            .ok_or_else(|| TurnstileError::Settlement("settlement record not found".into()))
    }
}

#[async_trait]
impl SettlementJournal for PostgresSettlementJournal {
    async fn begin_verification(
        &self,
        authorization: &PaymentAuthorization,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        let authorization_json = encode(authorization)?;
        sqlx::query(
            "INSERT INTO turnstile_settlements \
             (nonce, authorization_json, phase, owner_token, lease_until_ms) \
             VALUES ($1, $2, 'verifying', $3, $4) ON CONFLICT (nonce) DO NOTHING",
        )
        .bind(&authorization.nonce)
        .bind(authorization_json)
        .bind(owner_token)
        .bind(lease_until_ms)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        let record = self.required(&authorization.nonce).await?;
        if record.authorization != *authorization {
            return Err(TurnstileError::Settlement(
                "authorization nonce is bound to another settlement command".into(),
            ));
        }
        Ok(record)
    }

    async fn bind_verified(
        &self,
        nonce: &str,
        owner_token: &str,
        hold: Option<&HoldRef>,
        scheme: Scheme,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        if scheme == Scheme::Hold && hold.is_none() {
            return Err(TurnstileError::Settlement(
                "hold verification returned no hold identity".into(),
            ));
        }
        if scheme == Scheme::Transfer && hold.is_some() {
            return Err(TurnstileError::Settlement(
                "transfer verification unexpectedly returned a hold identity".into(),
            ));
        }
        let current = self.required(nonce).await?;
        if current.authorization.scheme != scheme {
            return Err(TurnstileError::Settlement(
                "Mint verification scheme differs from authorized scheme".into(),
            ));
        }
        let phase = if scheme == Scheme::Hold {
            "held"
        } else {
            "transfer_complete"
        };
        let scheme_json = encode(&scheme)?;
        let query = format!(
            "UPDATE turnstile_settlements SET phase = $3, hold_id = $4, scheme_json = $5, \
             lease_until_ms = $6, last_error = '', updated_at = NOW() \
             WHERE nonce = $1 AND owner_token = $2 AND phase = 'verifying' RETURNING {COLUMNS}"
        );
        sqlx::query_as::<_, StoredSettlement>(&query)
            .bind(nonce)
            .bind(owner_token)
            .bind(phase)
            .bind(hold.map(|value| value.0.as_str()))
            .bind(scheme_json)
            .bind(lease_until_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .map(TryInto::try_into)
            .transpose()?
            .ok_or_else(|| TurnstileError::Settlement("verified hold binding conflict".into()))
    }

    async fn renew(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<(), TurnstileError> {
        let result = sqlx::query(
            "UPDATE turnstile_settlements SET lease_until_ms = $3, updated_at = NOW() \
             WHERE nonce = $1 AND owner_token = $2 AND phase IN ('verifying', 'held')",
        )
        .bind(nonce)
        .bind(owner_token)
        .bind(lease_until_ms)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(TurnstileError::Settlement(
                "active settlement lease renewal conflict".into(),
            ))
        }
    }

    async fn prepare_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        actual: AmountMicros,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        let query = format!(
            "UPDATE turnstile_settlements SET phase = 'capture_pending', actual_micros = $3, \
             lease_until_ms = $4, last_error = '', updated_at = NOW() \
             WHERE nonce = $1 AND owner_token = $2 AND phase = 'held' \
             AND $3 > 0 AND $3 <= ((authorization_json::jsonb ->> 'amount_micros')::bigint) \
             RETURNING {COLUMNS}"
        );
        sqlx::query_as::<_, StoredSettlement>(&query)
            .bind(nonce)
            .bind(owner_token)
            .bind(actual.get())
            .bind(lease_until_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .map(TryInto::try_into)
            .transpose()?
            .ok_or_else(|| {
                TurnstileError::Settlement(
                    "capture intent conflicts with settlement state or quote".into(),
                )
            })
    }

    async fn prepare_release(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        let query = format!(
            "UPDATE turnstile_settlements SET phase = 'release_pending', actual_micros = NULL, \
             lease_until_ms = $3, last_error = '', updated_at = NOW() \
             WHERE nonce = $1 AND owner_token = $2 AND phase = 'held' RETURNING {COLUMNS}"
        );
        sqlx::query_as::<_, StoredSettlement>(&query)
            .bind(nonce)
            .bind(owner_token)
            .bind(lease_until_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .map(TryInto::try_into)
            .transpose()?
            .ok_or_else(|| {
                TurnstileError::Settlement("release intent conflicts with settlement state".into())
            })
    }

    async fn complete_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        receipt: &Receipt,
    ) -> Result<(), TurnstileError> {
        let receipt_json = encode(receipt)?;
        let result = sqlx::query(
            "UPDATE turnstile_settlements SET phase = 'captured', receipt_json = $3, \
             last_error = '', updated_at = NOW() WHERE nonce = $1 AND owner_token = $2 \
             AND phase = 'capture_pending' AND actual_micros = $4",
        )
        .bind(nonce)
        .bind(owner_token)
        .bind(receipt_json)
        .bind(receipt.amount_micros.get())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        require_one(
            result.rows_affected(),
            "capture receipt does not match pending intent",
        )
    }

    async fn complete_release(&self, nonce: &str, owner_token: &str) -> Result<(), TurnstileError> {
        let result = sqlx::query(
            "UPDATE turnstile_settlements SET phase = 'released', last_error = '', \
             updated_at = NOW() WHERE nonce = $1 AND owner_token = $2 \
             AND phase = 'release_pending'",
        )
        .bind(nonce)
        .bind(owner_token)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        require_one(
            result.rows_affected(),
            "release completion has no pending intent",
        )
    }

    async fn reject_verification(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError> {
        let result = sqlx::query(
            "UPDATE turnstile_settlements SET phase = 'rejected', last_error = $3, \
             updated_at = NOW() WHERE nonce = $1 AND owner_token = $2 AND phase = 'verifying'",
        )
        .bind(nonce)
        .bind(owner_token)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        require_one(
            result.rows_affected(),
            "verification rejection conflicts with settlement state",
        )
    }

    async fn record_error(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError> {
        let result = sqlx::query(
            "UPDATE turnstile_settlements SET last_error = $3, updated_at = NOW() \
             WHERE nonce = $1 AND owner_token = $2 AND phase NOT IN \
             ('captured', 'released', 'transfer_complete', 'rejected')",
        )
        .bind(nonce)
        .bind(owner_token)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        require_one(
            result.rows_affected(),
            "terminal or unowned settlement cannot record error",
        )
    }

    async fn claim_recoverable(
        &self,
        worker_token: &str,
        now_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        let query = format!(
            "WITH claimed AS (SELECT nonce FROM turnstile_settlements \
             WHERE phase NOT IN ('captured', 'released', 'transfer_complete', 'rejected') \
             AND lease_until_ms <= $1 ORDER BY lease_until_ms, nonce \
             FOR UPDATE SKIP LOCKED LIMIT $2) \
             UPDATE turnstile_settlements AS settlement SET owner_token = $3, \
             lease_until_ms = $4, updated_at = NOW() FROM claimed \
             WHERE settlement.nonce = claimed.nonce RETURNING {RETURNING_COLUMNS}"
        );
        let rows = sqlx::query_as::<_, StoredSettlement>(&query)
            .bind(now_ms)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .bind(worker_token)
            .bind(lease_until_ms)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn get(&self, nonce: &str) -> Result<Option<SettlementRecord>, TurnstileError> {
        self.fetch(nonce).await
    }

    async fn list_recent(
        &self,
        limit: usize,
        phase: Option<SettlementPhase>,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = match phase {
            Some(phase) => {
                let query = format!(
                    "SELECT {COLUMNS} FROM turnstile_settlements WHERE phase = $1 \
                     ORDER BY created_at DESC, nonce LIMIT $2"
                );
                sqlx::query_as::<_, StoredSettlement>(&query)
                    .bind(phase_tag(phase))
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                let query = format!(
                    "SELECT {COLUMNS} FROM turnstile_settlements \
                     ORDER BY created_at DESC, nonce LIMIT $1"
                );
                sqlx::query_as::<_, StoredSettlement>(&query)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(db)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

fn phase_tag(phase: SettlementPhase) -> &'static str {
    match phase {
        SettlementPhase::Verifying => "verifying",
        SettlementPhase::Held => "held",
        SettlementPhase::CapturePending => "capture_pending",
        SettlementPhase::Captured => "captured",
        SettlementPhase::ReleasePending => "release_pending",
        SettlementPhase::Released => "released",
        SettlementPhase::TransferComplete => "transfer_complete",
        SettlementPhase::Rejected => "rejected",
    }
}

fn require_one(rows: u64, message: &str) -> Result<(), TurnstileError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(TurnstileError::Settlement(message.into()))
    }
}

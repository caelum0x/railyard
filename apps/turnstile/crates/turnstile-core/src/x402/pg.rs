//! Postgres-backed [`X402Journal`] (table `turnstile_x402_payments`, migration
//! `0005_x402_payments.sql`). Runtime query API only, so the crate builds
//! without a `DATABASE_URL`.

use async_trait::async_trait;
use sqlx::postgres::PgPool;

use super::journal::{MintCreditStatus, X402Journal, X402Phase, X402Record, X402Summary};
use crate::clock::now_ms;
use crate::error::TurnstileError;

fn store_err(e: sqlx::Error) -> TurnstileError {
    TurnstileError::Storage(e.to_string())
}

#[derive(sqlx::FromRow)]
struct Row {
    nonce: String,
    resource: String,
    network: String,
    payer: String,
    pay_to: String,
    amount_micros: i64,
    phase: String,
    transaction: Option<String>,
    mint_status: String,
    mint_attempts: i32,
    last_error: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl TryFrom<Row> for X402Record {
    type Error = TurnstileError;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        Ok(Self {
            nonce: row.nonce,
            resource: row.resource,
            network: row.network,
            payer: row.payer,
            pay_to: row.pay_to,
            amount_micros: row.amount_micros,
            phase: X402Phase::parse(&row.phase)?,
            transaction: row.transaction,
            mint_status: MintCreditStatus::parse(&row.mint_status)?,
            mint_attempts: row.mint_attempts,
            last_error: row.last_error,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

const COLUMNS: &str =
    "nonce, resource, network, payer, pay_to, amount_micros, phase, transaction, \
     mint_status, mint_attempts, last_error, created_at_ms, updated_at_ms";

/// x402 journal on the Turnstile Postgres pool (shares the nonce store's pool).
#[derive(Clone)]
pub struct PostgresX402Journal {
    pool: PgPool,
}

impl PostgresX402Journal {
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn fetch(&self, sql: &str, limit: i64) -> Result<Vec<X402Record>, TurnstileError> {
        let rows: Vec<Row> = sqlx::query_as(sql)
            .bind(limit.clamp(0, 1000))
            .fetch_all(&self.pool)
            .await
            .map_err(store_err)?;
        rows.into_iter().map(X402Record::try_from).collect()
    }
}

#[async_trait]
impl X402Journal for PostgresX402Journal {
    async fn reserve(&self, record: X402Record) -> Result<(), TurnstileError> {
        let inserted = sqlx::query(
            "INSERT INTO turnstile_x402_payments \
             (nonce, resource, network, payer, pay_to, amount_micros, phase, transaction, \
              mint_status, mint_attempts, last_error, created_at_ms, updated_at_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, 0, '', $9, $9) \
             ON CONFLICT (nonce) DO NOTHING",
        )
        .bind(&record.nonce)
        .bind(&record.resource)
        .bind(&record.network)
        .bind(&record.payer)
        .bind(&record.pay_to)
        .bind(record.amount_micros)
        .bind(record.phase.as_str())
        .bind(record.mint_status.as_str())
        .bind(record.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if inserted.rows_affected() == 0 {
            return Err(TurnstileError::NonceReplayed(record.nonce));
        }
        Ok(())
    }

    async fn settle(
        &self,
        nonce: &str,
        transaction: &str,
        payer: &str,
        mint_status: MintCreditStatus,
    ) -> Result<(), TurnstileError> {
        let updated = sqlx::query(
            "UPDATE turnstile_x402_payments \
             SET phase = 'settled', transaction = $2, \
                 payer = CASE WHEN $3 = '' THEN payer ELSE $3 END, \
                 mint_status = $4, updated_at_ms = $5 \
             WHERE nonce = $1 AND phase = 'reserved'",
        )
        .bind(nonce)
        .bind(transaction)
        .bind(payer)
        .bind(mint_status.as_str())
        .bind(now_ms())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        if updated.rows_affected() == 0 {
            return Err(TurnstileError::Settlement(format!(
                "x402 payment {nonce} is not reserved"
            )));
        }
        Ok(())
    }

    async fn abandon(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError> {
        sqlx::query(
            "UPDATE turnstile_x402_payments \
             SET phase = 'abandoned', last_error = $2, updated_at_ms = $3 \
             WHERE nonce = $1 AND phase = 'reserved'",
        )
        .bind(nonce)
        .bind(reason)
        .bind(now_ms())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn mark_unknown(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError> {
        sqlx::query(
            "UPDATE turnstile_x402_payments \
             SET phase = 'unknown', last_error = $2, updated_at_ms = $3 \
             WHERE nonce = $1 AND phase = 'reserved'",
        )
        .bind(nonce)
        .bind(reason)
        .bind(now_ms())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn record_credit(
        &self,
        nonce: &str,
        status: MintCreditStatus,
        error: &str,
    ) -> Result<(), TurnstileError> {
        sqlx::query(
            "UPDATE turnstile_x402_payments \
             SET mint_status = $2, mint_attempts = mint_attempts + 1, last_error = $3, \
                 updated_at_ms = $4 \
             WHERE nonce = $1 AND phase = 'settled'",
        )
        .bind(nonce)
        .bind(status.as_str())
        .bind(error)
        .bind(now_ms())
        .execute(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn pending_credits(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError> {
        self.fetch(
            &format!(
                "SELECT {COLUMNS} FROM turnstile_x402_payments \
                 WHERE phase = 'settled' AND mint_status = 'pending' \
                 ORDER BY updated_at_ms ASC LIMIT $1"
            ),
            limit,
        )
        .await
    }

    async fn recent(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError> {
        self.fetch(
            &format!(
                "SELECT {COLUMNS} FROM turnstile_x402_payments \
                 ORDER BY created_at_ms DESC LIMIT $1"
            ),
            limit,
        )
        .await
    }

    async fn summary(&self) -> Result<X402Summary, TurnstileError> {
        let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT \
               COUNT(*) FILTER (WHERE phase = 'settled'), \
               COALESCE(SUM(amount_micros) FILTER (WHERE phase = 'settled'), 0)::BIGINT, \
               COALESCE(SUM(amount_micros) FILTER (WHERE phase = 'settled' AND mint_status = 'credited'), 0)::BIGINT, \
               COUNT(*) FILTER (WHERE phase = 'settled' AND mint_status = 'pending'), \
               COUNT(*) FILTER (WHERE phase = 'settled' AND mint_status = 'failed'), \
               COUNT(*) FILTER (WHERE phase = 'abandoned'), \
               COUNT(*) FILTER (WHERE phase = 'unknown') \
             FROM turnstile_x402_payments",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(store_err)?;
        Ok(X402Summary {
            settled_count: row.0,
            settled_micros: row.1,
            credited_micros: row.2,
            pending_credit_count: row.3,
            failed_credit_count: row.4,
            abandoned_count: row.5,
            unknown_count: row.6,
        })
    }
}

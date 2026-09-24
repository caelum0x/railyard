//! Persistence for real-time (Mint-hold backed) wallets.

use chrono::{NaiveDateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// A customer's real-time wallet configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeWallet {
    pub wallet_id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub currency: String,
    /// `reject` | `flag`.
    pub policy: String,
    pub mint_account_id: String,
}

/// One reserved (or flagged) event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeHold {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub wallet_id: Uuid,
    pub customer_id: Uuid,
    pub subscription_id: Uuid,
    pub event_transaction_id: String,
    pub event_timestamp: NaiveDateTime,
    pub mint_hold_id: Option<String>,
    pub amount_cents: i64,
    pub amount_micros: i64,
    /// `open` | `captured` | `released` | `flagged`.
    pub status: String,
    pub invoice_id: Option<Uuid>,
    pub captured_micros: i64,
}

fn hold_row(r: &sqlx::postgres::PgRow) -> Result<RealtimeHold> {
    Ok(RealtimeHold {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        wallet_id: r.try_get("wallet_id")?,
        customer_id: r.try_get("customer_id")?,
        subscription_id: r.try_get("subscription_id")?,
        event_transaction_id: r.try_get("event_transaction_id")?,
        event_timestamp: r.try_get("event_timestamp")?,
        mint_hold_id: r.try_get("mint_hold_id")?,
        amount_cents: r.try_get("amount_cents")?,
        amount_micros: r.try_get("amount_micros")?,
        status: r.try_get("status")?,
        invoice_id: r.try_get("invoice_id")?,
        captured_micros: r.try_get("captured_micros")?,
    })
}

const HOLD_COLUMNS: &str = "id, organization_id, wallet_id, customer_id, subscription_id, \
    event_transaction_id, event_timestamp, mint_hold_id, amount_cents, amount_micros, status, \
    invoice_id, captured_micros";

impl Db {
    /// The customer's active real-time wallet (highest priority first).
    pub async fn realtime_wallet_for_customer(&self, customer_id: Uuid) -> Result<Option<RealtimeWallet>> {
        let row = sqlx::query(
            "SELECT id, organization_id, customer_id, balance_currency, realtime_policy, mint_account_id \
             FROM wallets WHERE customer_id = $1 AND status = 0 AND realtime_enabled \
               AND mint_account_id IS NOT NULL AND mint_account_id <> '' \
             ORDER BY priority ASC, created_at ASC LIMIT 1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<RealtimeWallet> {
            Ok(RealtimeWallet {
                wallet_id: r.try_get("id")?,
                organization_id: r.try_get("organization_id")?,
                customer_id: r.try_get("customer_id")?,
                currency: r.try_get("balance_currency")?,
                policy: r.try_get("realtime_policy")?,
                mint_account_id: r.try_get("mint_account_id")?,
            })
        })
        .transpose()
    }

    /// Ids of real-time wallets (excluded from invoice-time prepaid draw-down:
    /// their money is captured from Mint instead).
    pub async fn realtime_wallet_ids(&self, customer_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query("SELECT id FROM wallets WHERE customer_id = $1 AND realtime_enabled")
            .bind(customer_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
    }

    /// Configure real-time debit on a wallet of `organization_id`.
    pub async fn set_wallet_realtime(
        &self,
        organization_id: Uuid,
        wallet_id: Uuid,
        enabled: bool,
        policy: &str,
        mint_account_id: Option<&str>,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE wallets SET realtime_enabled = $3, realtime_policy = $4, \
                mint_account_id = COALESCE($5, mint_account_id), updated_at = $6 \
             WHERE id = $1 AND organization_id = $2",
        )
        .bind(wallet_id)
        .bind(organization_id)
        .bind(enabled)
        .bind(policy)
        .bind(mint_account_id)
        .bind(Utc::now().naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// `(realtime_enabled, realtime_policy, mint_account_id)` of a wallet.
    pub async fn wallet_realtime_settings(&self, wallet_id: Uuid) -> Result<Option<(bool, String, Option<String>)>> {
        let row = sqlx::query("SELECT realtime_enabled, realtime_policy, mint_account_id FROM wallets WHERE id = $1")
            .bind(wallet_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| -> Result<(bool, String, Option<String>)> {
            Ok((r.try_get("realtime_enabled")?, r.try_get("realtime_policy")?, r.try_get("mint_account_id")?))
        })
        .transpose()
    }

    /// Record a reservation. `false` when this wallet already has a row for
    /// the event (a replay).
    pub async fn insert_realtime_hold(&self, h: &RealtimeHold, last_error: Option<&str>) -> Result<bool> {
        let now = Utc::now().naive_utc();
        let r = sqlx::query(
            "INSERT INTO wallet_realtime_holds (id, organization_id, wallet_id, customer_id, subscription_id, \
                event_transaction_id, event_timestamp, mint_hold_id, amount_cents, amount_micros, status, \
                invoice_id, captured_micros, last_error, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$15) \
             ON CONFLICT (wallet_id, event_transaction_id) DO NOTHING",
        )
        .bind(h.id)
        .bind(h.organization_id)
        .bind(h.wallet_id)
        .bind(h.customer_id)
        .bind(h.subscription_id)
        .bind(&h.event_transaction_id)
        .bind(h.event_timestamp)
        .bind(&h.mint_hold_id)
        .bind(h.amount_cents)
        .bind(h.amount_micros)
        .bind(&h.status)
        .bind(h.invoice_id)
        .bind(h.captured_micros)
        .bind(last_error)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The reservation of an event on a wallet, if any.
    pub async fn realtime_hold_for_event(&self, wallet_id: Uuid, transaction_id: &str) -> Result<Option<RealtimeHold>> {
        let row = sqlx::query(&format!(
            "SELECT {HOLD_COLUMNS} FROM wallet_realtime_holds WHERE wallet_id = $1 AND event_transaction_id = $2"
        ))
        .bind(wallet_id)
        .bind(transaction_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(hold_row).transpose()
    }

    /// Open holds for the subscriptions an invoice bills, for events up to each
    /// subscription's charges period end, oldest first.
    pub async fn open_realtime_holds_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<RealtimeHold>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM wallet_realtime_holds h \
             JOIN invoice_subscriptions s ON s.subscription_id = h.subscription_id \
             WHERE s.invoice_id = $1 AND h.status = 'open' \
               AND (s.charges_to_datetime IS NULL OR h.event_timestamp <= s.charges_to_datetime) \
             ORDER BY h.created_at ASC, h.id ASC",
            HOLD_COLUMNS.split(", ").map(|c| format!("h.{}", c.trim())).collect::<Vec<_>>().join(", ")
        ))
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(hold_row).collect()
    }

    /// Holds bound to an invoice that are still open (a capture that did not
    /// complete), for release on void.
    pub async fn open_realtime_holds_bound_to_invoice(&self, invoice_id: Uuid) -> Result<Vec<RealtimeHold>> {
        let rows = sqlx::query(&format!(
            "SELECT {HOLD_COLUMNS} FROM wallet_realtime_holds WHERE invoice_id = $1 AND status = 'open' ORDER BY created_at"
        ))
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(hold_row).collect()
    }

    /// Move a reservation to its settled state.
    pub async fn settle_realtime_hold(
        &self,
        id: Uuid,
        status: &str,
        invoice_id: Option<Uuid>,
        captured_micros: i64,
        last_error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE wallet_realtime_holds SET status = $2, invoice_id = COALESCE($3, invoice_id), \
                captured_micros = $4, last_error = $5, updated_at = $6 WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(invoice_id)
        .bind(captured_micros)
        .bind(last_error)
        .bind(Utc::now().naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bind still-open holds to the invoice about to capture them, so a void
    /// can find them even if the capture is interrupted.
    pub async fn bind_realtime_holds_to_invoice(&self, ids: &[Uuid], invoice_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE wallet_realtime_holds SET invoice_id = $2 WHERE id = ANY($1) AND status = 'open'")
            .bind(ids)
            .bind(invoice_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Apply money captured from Mint to a finalized invoice as prepaid credit:
    /// the total drops by `cents` (never below zero) and a fully covered
    /// invoice is marked paid. Returns the applied amount.
    pub async fn apply_realtime_capture_to_invoice(&self, invoice_id: Uuid, cents: i64) -> Result<i64> {
        let row = sqlx::query(
            "WITH cur AS (SELECT id, LEAST($2, total_amount_cents) AS applied \
                          FROM invoices WHERE id = $1 FOR UPDATE) \
             UPDATE invoices i SET \
                prepaid_credit_amount_cents = i.prepaid_credit_amount_cents + cur.applied, \
                total_amount_cents = i.total_amount_cents - cur.applied, \
                payment_status = CASE WHEN i.total_amount_cents - cur.applied = 0 THEN 1 ELSE i.payment_status END, \
                updated_at = $3 \
             FROM cur WHERE i.id = cur.id AND cur.applied > 0 \
             RETURNING cur.applied AS applied",
        )
        .bind(invoice_id)
        .bind(cents)
        .bind(Utc::now().naive_utc())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.try_get::<i64, _>("applied")).transpose()?.unwrap_or(0))
    }
}

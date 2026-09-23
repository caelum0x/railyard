//! Purchased (paid) wallet credits: the pending transaction + its credit
//! invoice are written together, and the credits only reach the wallet
//! balance once that invoice is paid (`WalletTransactions::SettleService`).

use rago_core::enums::{WalletTransactionStatus, WalletTransactionType};
use rago_core::models::{Fee, Invoice, WalletTransaction};
use rago_core::money::Currency;
use rust_decimal::prelude::ToPrimitive;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::{insert_fee_exec, insert_invoice_exec, Db};

impl Db {
    /// Write a paid-credit purchase atomically: the (numbered) credit invoice,
    /// its fee, and the pending inbound wallet transaction it settles.
    pub async fn insert_paid_credit_purchase(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
        wallet_transaction: &WalletTransaction,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        crate::numbering::number_if_needed(&mut *tx, invoice).await?;
        insert_invoice_exec(&mut *tx, invoice).await?;
        for fee in fees {
            insert_fee_exec(&mut *tx, fee).await?;
        }
        sqlx::query(
            "INSERT INTO wallet_transactions (id, organization_id, wallet_id, transaction_type, status, \
             amount, credit_amount, invoice_id, settled_at, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(wallet_transaction.id)
        .bind(wallet_transaction.organization_id)
        .bind(wallet_transaction.wallet_id)
        .bind(wallet_transaction.transaction_type.as_i32())
        .bind(wallet_transaction.status.as_i32())
        .bind(wallet_transaction.amount)
        .bind(wallet_transaction.credit_amount)
        .bind(Some(invoice.id))
        .bind(wallet_transaction.settled_at.map(|t| t.naive_utc()))
        .bind(wallet_transaction.created_at.naive_utc())
        .bind(wallet_transaction.updated_at.naive_utc())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Settle the paid-credit transactions of a paid invoice and add their
    /// credits to the wallet balances — exactly once (only rows not yet
    /// `settled` move; a failed purchase settles when a retry succeeds).
    /// Returns the settled transactions.
    pub async fn settle_paid_credit_transactions(&self, invoice_id: Uuid) -> Result<Vec<WalletTransaction>> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "UPDATE wallet_transactions SET status = $2, settled_at = now(), updated_at = now() \
             WHERE invoice_id = $1 AND status <> $2 AND transaction_type = $3 RETURNING *",
        )
        .bind(invoice_id)
        .bind(WalletTransactionStatus::Settled.as_i32())
        .bind(WalletTransactionType::Inbound.as_i32())
        .fetch_all(&mut *tx)
        .await?;
        let settled: Vec<WalletTransaction> =
            rows.iter().map(mapping::wallet_transaction).collect::<Result<_>>()?;
        for wtx in &settled {
            let currency: Option<String> = sqlx::query(
                "SELECT balance_currency FROM wallets WHERE id = $1 FOR UPDATE",
            )
            .bind(wtx.wallet_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.try_get::<String, _>("balance_currency"))
            .transpose()?;
            let Some(currency) = currency else { continue };
            let cents = (wtx.amount * Currency::new(&currency).subunit_to_unit_decimal())
                .round()
                .to_i64()
                .unwrap_or(0);
            sqlx::query(
                "UPDATE wallets SET balance_cents = balance_cents + $2, \
                 credits_balance = credits_balance + $3, updated_at = now() WHERE id = $1",
            )
            .bind(wtx.wallet_id)
            .bind(cents)
            .bind(wtx.credit_amount)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(settled)
    }

    /// Mark the pending paid-credit transactions of an unpaid invoice failed
    /// (no credits are granted). Returns the failed transactions.
    pub async fn fail_paid_credit_transactions(&self, invoice_id: Uuid) -> Result<Vec<WalletTransaction>> {
        let rows = sqlx::query(
            "UPDATE wallet_transactions SET status = $2, updated_at = now() \
             WHERE invoice_id = $1 AND status = $3 AND transaction_type = $4 RETURNING *",
        )
        .bind(invoice_id)
        .bind(WalletTransactionStatus::Failed.as_i32())
        .bind(WalletTransactionStatus::Pending.as_i32())
        .bind(WalletTransactionType::Inbound.as_i32())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::wallet_transaction).collect()
    }
}

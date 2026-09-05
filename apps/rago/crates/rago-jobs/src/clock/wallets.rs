//! Wallet-lifecycle clocks.
//!
//! Ports `Clock::{TerminateWallets, CreateIntervalWalletTransactions,
//! TerminateRecurringTransactionRules, RefreshWalletsOngoingBalance}Job`.

use chrono::Utc;
use rago_core::enums::{WalletStatus, WalletTransactionStatus, WalletTransactionType};
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::webhook_emit::record_webhook;
use crate::JobError;

/// `Clock::TerminateWalletsJob` → per-wallet `Wallets::TerminateService`, which
/// emits `wallet.terminated` after commit. Active wallets past their expiration
/// are terminated. Returns the count terminated.
///
/// The terminated rows are captured with `RETURNING`, so a re-run selects
/// nothing and the webhook fires exactly once per wallet.
pub async fn terminate_wallets(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let rows = sqlx::query(
        "UPDATE wallets SET status = $1, terminated_at = now(), updated_at = now() \
         WHERE status = $2 AND expiration_at IS NOT NULL AND expiration_at <= $3 \
         RETURNING id, organization_id",
    )
    .bind(WalletStatus::Terminated.as_i32())
    .bind(WalletStatus::Active.as_i32())
    .bind(now)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    for row in &rows {
        let id: Uuid = row.get("id");
        let org: Uuid = row.get("organization_id");
        record_webhook(
            db,
            org,
            WebhookKind::WalletTerminated,
            json!({ "lago_id": id, "status": "terminated" }),
        )
        .await?;
    }
    Ok(rows.len() as u64)
}

/// `Clock::TerminateRecurringTransactionRulesJob` →
/// `Wallets::RecurringTransactionRules::TerminateService`. Active rules past
/// their expiration are terminated. Returns the count terminated.
pub async fn terminate_recurring_transaction_rules(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let r = sqlx::query(
        "UPDATE wallet_recurring_rules SET status = 1, terminated_at = now(), updated_at = now() \
         WHERE status = 0 AND expiration_at IS NOT NULL AND expiration_at <= $1",
    )
    .bind(now)
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::RefreshWalletsOngoingBalanceJob` → `Wallets::Balance::UpdateOngoingService`.
/// Recomputes each active wallet's ongoing balance from its settled balance
/// minus the live (unsettled) usage, both in credits and cents. Idempotent.
/// Returns the count refreshed.
///
/// Mirrors `UpdateOngoingService`, which emits `wallet.depleted_ongoing_balance`
/// only when `depleted_ongoing_balance == true` — i.e. on the *transition* into
/// depletion. We reproduce that gate by snapshotting each wallet's prior ongoing
/// balance and emitting only for wallets that crossed from positive to `<= 0`
/// this tick, so a wallet that was already depleted does not re-alert.
pub async fn refresh_wallets_ongoing_balance(db: &Db) -> Result<u64, JobError> {
    let rows = sqlx::query(
        "WITH snap AS ( \
             SELECT id, ongoing_balance_cents AS old_ongoing FROM wallets WHERE status = $1 \
         ), upd AS ( \
             UPDATE wallets w SET \
                 ongoing_balance_cents = w.balance_cents - w.ongoing_usage_balance_cents, \
                 credits_ongoing_balance = w.credits_balance - w.credits_ongoing_usage_balance, \
                 updated_at = now() \
             WHERE w.status = $1 \
             RETURNING id, organization_id, ongoing_balance_cents AS new_ongoing \
         ) \
         SELECT upd.id, upd.organization_id, upd.new_ongoing, snap.old_ongoing \
         FROM upd JOIN snap ON snap.id = upd.id",
    )
    .bind(WalletStatus::Active.as_i32())
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    for row in &rows {
        let new_ongoing: i64 = row.get("new_ongoing");
        let old_ongoing: i64 = row.get("old_ongoing");
        // Depletion transition: was positive, now non-positive.
        if new_ongoing <= 0 && old_ongoing > 0 {
            let id: Uuid = row.get("id");
            let org: Uuid = row.get("organization_id");
            record_webhook(
                db,
                org,
                WebhookKind::WalletDepletedOngoingBalance,
                json!({ "lago_id": id, "ongoing_balance_cents": new_ongoing }),
            )
            .await?;
        }
    }
    Ok(rows.len() as u64)
}

/// `Clock::CreateIntervalWalletTransactionsJob` →
/// `Wallets::CreateIntervalWalletTransactionsService`. For each active
/// interval-based recurring top-up rule whose `next_top_up_at` has arrived,
/// credit the wallet (settled inbound transaction) and advance `next_top_up_at`
/// by one interval. Returns the number of top-ups performed.
///
/// Money-correctness + idempotency: `next_top_up_at` is advanced in the same
/// pass, so a second run inside the same interval window tops up nothing (the
/// rule is no longer due).
pub async fn create_interval_wallet_transactions(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let due = sqlx::query(
        "SELECT r.id, r.interval \
         FROM wallet_recurring_rules r JOIN wallets w ON w.id = r.wallet_id \
         WHERE r.rule_type = 'interval' AND r.status = 0 \
           AND r.next_top_up_at IS NOT NULL AND r.next_top_up_at <= $1 \
           AND w.status = $2",
    )
    .bind(now)
    .bind(WalletStatus::Active.as_i32())
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let mut topped_up: u64 = 0;
    for row in &due {
        let rule_id: Uuid = row.get("id");
        let interval: Option<String> = row.get("interval");
        let step = interval_step(interval.as_deref());

        // 1. Record the settled inbound top-up transaction.
        let txn = sqlx::query(
            "INSERT INTO wallet_transactions \
                 (id, organization_id, wallet_id, transaction_type, status, amount, credit_amount, settled_at, created_at, updated_at) \
             SELECT gen_random_uuid(), r.organization_id, r.wallet_id, $2, $3, \
                    (r.paid_credits + r.granted_credits) * w.rate_amount, \
                    (r.paid_credits + r.granted_credits), now(), now(), now() \
             FROM wallet_recurring_rules r JOIN wallets w ON w.id = r.wallet_id \
             WHERE r.id = $1 \
             RETURNING id, organization_id, wallet_id, credit_amount",
        )
        .bind(rule_id)
        .bind(WalletTransactionType::Inbound.as_i32())
        .bind(WalletTransactionStatus::Settled.as_i32())
        .fetch_one(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;

        // 2. Credit the wallet balance (credits + cents).
        sqlx::query(
            "UPDATE wallets w SET \
                 credits_balance = w.credits_balance + (r.paid_credits + r.granted_credits), \
                 balance_cents = w.balance_cents + round((r.paid_credits + r.granted_credits) * w.rate_amount * 100)::bigint, \
                 credits_ongoing_balance = w.credits_ongoing_balance + (r.paid_credits + r.granted_credits), \
                 ongoing_balance_cents = w.ongoing_balance_cents + round((r.paid_credits + r.granted_credits) * w.rate_amount * 100)::bigint, \
                 updated_at = now() \
             FROM wallet_recurring_rules r \
             WHERE r.id = $1 AND w.id = r.wallet_id",
        )
        .bind(rule_id)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;

        // 3. Advance the schedule (idempotency gate).
        sqlx::query(
            "UPDATE wallet_recurring_rules SET next_top_up_at = next_top_up_at + $2::interval, updated_at = now() WHERE id = $1",
        )
        .bind(rule_id)
        .bind(step)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;

        // 4. `wallet_transaction.created` — mirrors
        // `WalletTransactions::CreateFromParamsService`, which the interval
        // top-up funnels through. Fires once per top-up (idempotent, since a
        // re-run inside the interval selects no due rule).
        let txn_id: Uuid = txn.get("id");
        let org: Uuid = txn.get("organization_id");
        let wallet_id: Uuid = txn.get("wallet_id");
        let credit_amount: rust_decimal::Decimal = txn.get("credit_amount");
        record_webhook(
            db,
            org,
            WebhookKind::WalletTransactionCreated,
            json!({
                "lago_id": txn_id,
                "lago_wallet_id": wallet_id,
                "status": "settled",
                "transaction_type": "inbound",
                "amount": credit_amount.to_string(),
            }),
        )
        .await?;

        topped_up += 1;
    }
    Ok(topped_up)
}

/// Map a recurring-rule interval to a Postgres interval literal.
fn interval_step(interval: Option<&str>) -> &'static str {
    match interval {
        Some("weekly") => "7 days",
        Some("quarterly") => "3 months",
        Some("yearly") => "1 year",
        _ => "1 month", // monthly default
    }
}

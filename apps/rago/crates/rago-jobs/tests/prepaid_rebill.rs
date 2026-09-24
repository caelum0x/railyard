// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Prepaid wallet credit is applied to recurring invoices — and applied
//! *exactly once* across a re-bill.
//!
//! Regression for the billing-math gap: the scheduled billing path used to
//! ignore prepaid wallet balances, so a customer holding prepaid credit was
//! over-billed. It now draws the wallet down (after tax) and reduces the
//! invoice total. Because the draw-down commits inside the same transaction as
//! the re-bill guard, billing the same period twice must leave the wallet
//! debited by the same amount only once — conservation across the guard.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_core::enums::{
    BillingTime, PlanInterval, SubscriptionStatus, WalletStatus,
};
use rago_core::models::{Customer, Organization, Plan, Subscription, Wallet};
use rago_db::Db;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

fn organization(id: Uuid) -> Organization {
    let now = Utc::now();
    Organization {
        id,
        name: "Acme".into(),
        slug: format!("acme-{}", &id.to_string()[..8]),
        default_currency: "USD".into(),
        email: None,
        country: None,
        tax_identification_number: None,
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: true,
        hmac_key: Some("test-key".into()),
        created_at: now,
        updated_at: now,
    }
}

fn customer(id: Uuid, org: Uuid) -> Customer {
    let now = Utc::now();
    Customer {
        id,
        organization_id: org,
        billing_entity_id: org,
        external_id: format!("cust-{}", &id.to_string()[..8]),
        name: Some("Wile E".into()),
        email: None,
        country: None,
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn plan(id: Uuid, org: Uuid) -> Plan {
    let now = Utc::now();
    Plan {
        id,
        organization_id: org,
        name: "Pro".into(),
        code: format!("pro-{}", &id.to_string()[..8]),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(1000),
        amount_currency: "USD".into(),
        trial_period: None,
        pay_in_advance: false,
        bill_charges_monthly: None,
        parent_id: None,
        invoice_display_name: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn subscription(id: Uuid, org: Uuid, customer_id: Uuid, plan_id: Uuid) -> Subscription {
    let now = Utc::now();
    Subscription {
        id,
        organization_id: org,
        customer_id,
        plan_id,
        billing_entity_id: Some(org),
        external_id: format!("sub-{}", &id.to_string()[..8]),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        started_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn wallet(id: Uuid, org: Uuid, customer_id: Uuid, balance_cents: i64) -> Wallet {
    let now = Utc::now();
    Wallet {
        id,
        organization_id: org,
        customer_id,
        status: WalletStatus::Active,
        name: None,
        rate_amount: Decimal::ONE,
        credits_balance: Decimal::from(balance_cents) / Decimal::from(100),
        consumed_credits: Decimal::ZERO,
        balance_cents,
        balance_currency: "USD".into(),
        allowed_fee_types: Vec::new(),
        consumed_amount_cents: 0,
        ongoing_balance_cents: 0,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 50,
        created_at: now,
        updated_at: now,
    }
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn prepaid_credit_applied_once_across_rebill(pool: PgPool) {
    let db = Db::new(pool);

    let org = Uuid::new_v4();
    let cust = Uuid::new_v4();
    let pln = Uuid::new_v4();
    let sub = Uuid::new_v4();
    let wal = Uuid::new_v4();

    db.insert_organization(&organization(org)).await.unwrap();
    db.insert_customer(&customer(cust, org)).await.unwrap();
    db.insert_plan(&plan(pln, org)).await.unwrap();
    db.insert_subscription(&subscription(sub, org, cust, pln)).await.unwrap();
    // $50 of prepaid credit; the flat plan fee is $10 (1000c).
    db.insert_wallet(&wallet(wal, org, cust, 5000)).await.unwrap();

    let from = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    let to = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap().and_hms_opt(23, 59, 59).unwrap();

    let run1 = rago_jobs::bill_subscription(&db, sub, from, to).await.unwrap();
    // The $10 fee is fully covered by prepaid credit -> total 0, prepaid 1000c.
    assert_eq!(run1.invoice.prepaid_credit_amount_cents, 1000);
    assert_eq!(run1.invoice.total_amount_cents, 0);

    // Second run for the same period hits the re-bill guard and must NOT draw
    // the wallet again.
    let run2 = rago_jobs::bill_subscription(&db, sub, from, to).await.unwrap();
    assert_eq!(run1.invoice.id, run2.invoice.id);

    // Exactly one invoice and exactly one outbound wallet transaction.
    let invoices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id = $1")
        .bind(cust)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(invoices, 1, "exactly one invoice for the subscription+period");

    let wtx: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM wallet_transactions WHERE wallet_id = $1")
            .bind(wal)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(wtx, 1, "wallet drawn down exactly once across the re-bill");

    // Conservation: the wallet is debited by exactly the prepaid credit applied
    // (5000c - 1000c = 4000c), and never twice.
    let balance: i64 = sqlx::query_scalar("SELECT balance_cents FROM wallets WHERE id = $1")
        .bind(wal)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(balance, 4000, "wallet balance decremented by the prepaid credit, once");
}

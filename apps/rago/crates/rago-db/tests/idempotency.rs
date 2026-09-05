// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Money-correctness idempotency guards (audit fixes #2 and #3), proven against
//! a real Postgres via `#[sqlx::test]` (each test runs on an isolated, migrated
//! database). These are the DB-layer guarantees the billing/event paths rely on:
//!
//!   * usage-event dedup: two identical `(org, external_subscription, transaction)`
//!     events yield exactly one row; the second insert is an idempotent no-op.
//!   * re-bill guard: two subscription-invoice writes for the same
//!     `(subscription, charges period)` yield exactly one invoice; the second
//!     returns the existing invoice instead of a duplicate.

use chrono::{NaiveDate, Utc};
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Event, Invoice};
use rago_db::{BilledInvoice, Db};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn event(org: Uuid, external_subscription_id: &str, transaction_id: &str) -> Event {
    let now = Utc::now();
    Event {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: None,
        subscription_id: None,
        transaction_id: transaction_id.to_string(),
        code: "api_calls".to_string(),
        properties: json!({}),
        external_customer_id: None,
        external_subscription_id: Some(external_subscription_id.to_string()),
        timestamp: Some(now.naive_utc()),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

fn blank_invoice(org: Uuid, customer: Uuid) -> Invoice {
    let now = Utc::now();
    Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".to_string()),
        issuing_date: Some(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()),
        fees_amount_cents: 1000,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 1000,
        sub_total_including_taxes_amount_cents: 1000,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 1000,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

#[sqlx::test]
async fn insert_event_dedups_identical_transaction(pool: PgPool) -> sqlx::Result<()> {
    let db = Db::new(pool);
    let org = Uuid::new_v4();

    let first = db
        .insert_event(&event(org, "sub_1", "txn_1"))
        .await
        .unwrap();
    let second = db
        .insert_event(&event(org, "sub_1", "txn_1"))
        .await
        .unwrap();

    assert!(first, "first insert writes the row");
    assert!(
        !second,
        "identical (org, ext_sub, txn) is an idempotent no-op, not an error"
    );

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE organization_id = $1 AND transaction_id = 'txn_1'",
    )
    .bind(org)
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(count, 1, "exactly one event row survives the duplicate");
    Ok(())
}

#[sqlx::test]
async fn insert_event_allows_distinct_transactions(pool: PgPool) -> sqlx::Result<()> {
    let db = Db::new(pool);
    let org = Uuid::new_v4();

    // Different transaction_id, and same txn under a different subscription, both write.
    assert!(db
        .insert_event(&event(org, "sub_1", "txn_a"))
        .await
        .unwrap());
    assert!(db
        .insert_event(&event(org, "sub_1", "txn_b"))
        .await
        .unwrap());
    assert!(db
        .insert_event(&event(org, "sub_2", "txn_a"))
        .await
        .unwrap());

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE organization_id = $1")
        .bind(org)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count, 3);
    Ok(())
}

#[sqlx::test]
async fn rebill_guard_yields_single_invoice_per_period(pool: PgPool) -> sqlx::Result<()> {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let customer = Uuid::new_v4();
    let subscription = Uuid::new_v4();
    let from = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let to = NaiveDate::from_ymd_opt(2026, 1, 31)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();

    // First billing run creates the invoice.
    let invoice_a = blank_invoice(org, customer);
    let r1 = db
        .insert_subscription_invoice_guarded(&invoice_a, &[], subscription, from, to)
        .await
        .unwrap();
    assert!(
        matches!(r1, BilledInvoice::Created),
        "first run creates the invoice"
    );

    // Second run for the same subscription+period must NOT create a duplicate.
    let invoice_b = blank_invoice(org, customer); // distinct id, same period
    let r2 = db
        .insert_subscription_invoice_guarded(&invoice_b, &[], subscription, from, to)
        .await
        .unwrap();
    match r2 {
        BilledInvoice::AlreadyBilled(existing) => {
            assert_eq!(
                existing.id, invoice_a.id,
                "re-run returns the original invoice"
            );
        }
        BilledInvoice::Created => panic!("re-run must not create a second invoice"),
    }

    let invoices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id = $1")
        .bind(customer)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(invoices, 1, "exactly one invoice for the period");

    let guards: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM invoice_subscriptions WHERE subscription_id = $1")
            .bind(subscription)
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(guards, 1, "exactly one invoice_subscription guard row");
    Ok(())
}

#[sqlx::test]
async fn rebill_guard_allows_distinct_periods(pool: PgPool) -> sqlx::Result<()> {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let customer = Uuid::new_v4();
    let subscription = Uuid::new_v4();

    let jan_from = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let jan_to = NaiveDate::from_ymd_opt(2026, 1, 31)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();
    let feb_from = NaiveDate::from_ymd_opt(2026, 2, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let feb_to = NaiveDate::from_ymd_opt(2026, 2, 28)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();

    let jan = db
        .insert_subscription_invoice_guarded(
            &blank_invoice(org, customer),
            &[],
            subscription,
            jan_from,
            jan_to,
        )
        .await
        .unwrap();
    let feb = db
        .insert_subscription_invoice_guarded(
            &blank_invoice(org, customer),
            &[],
            subscription,
            feb_from,
            feb_to,
        )
        .await
        .unwrap();

    assert!(matches!(jan, BilledInvoice::Created));
    assert!(
        matches!(feb, BilledInvoice::Created),
        "a new period bills normally"
    );

    let invoices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id = $1")
        .bind(customer)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(invoices, 2, "distinct periods produce distinct invoices");
    Ok(())
}

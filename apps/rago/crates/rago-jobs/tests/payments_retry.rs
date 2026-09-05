// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Charge-level retry idempotency (audit fix #1), through the real
//! `payments::charge_invoice` path against a migrated Postgres.
//!
//! Retrying a charge for the SAME invoice must never reach the provider a
//! second time once a non-failed payment is recorded (the app-level guard —
//! Stripe's idempotency keys expire after ~24h, so the key alone is not
//! enough), must use the stable derived `Idempotency-Key` when it does call
//! Stripe, and must record exactly one `payments` row.

use chrono::{NaiveDate, Utc};
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::Invoice;
use rago_db::Db;
use rago_integrations::{MockTransport, PayableStatus};
use rago_jobs::payments::{charge_invoice, deterministic_payment_id};
use sqlx::PgPool;
use uuid::Uuid;

fn finalized_invoice(org: Uuid, customer: Uuid) -> Invoice {
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

/// Extract the `Idempotency-Key` header from a recorded Stripe request.
fn idempotency_key(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(k, _)| k == "Idempotency-Key")
        .map(|(_, v)| v.as_str())
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn double_charge_invoice_reuses_key_and_records_single_payment(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let customer = Uuid::new_v4();

    let invoice = finalized_invoice(org, customer);
    db.insert_invoice(&invoice).await.unwrap();

    // Stripe always accepts; the transport records every outbound request.
    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);

    // Charge the SAME invoice twice — a scheduler/job re-run of the payment.
    let first = charge_invoice(&db, &transport, "sk_test_x", &invoice, "cus_1")
        .await
        .unwrap();
    let second = charge_invoice(&db, &transport, "sk_test_x", &invoice, "cus_1")
        .await
        .unwrap();
    assert_eq!(first, PayableStatus::Succeeded);
    assert_eq!(second, PayableStatus::Succeeded);

    // (a) Only the FIRST attempt reaches the provider. The recorded succeeded
    // payment short-circuits the re-run before any provider call — the CRITICAL
    // double-charge fix (Stripe's idempotency key alone expires after ~24h, so
    // a late re-run would otherwise mint a fresh PaymentIntent).
    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "re-run never reaches the provider");
    let expected = format!("payment-{}", deterministic_payment_id(invoice.id));
    let key1 = idempotency_key(&requests[0].headers).expect("first request has Idempotency-Key");
    assert_eq!(key1, expected, "key derives from the invoice id");

    // (b) Exactly one payments row on the deterministic primary key.
    let payments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE invoice_id = $1")
        .bind(invoice.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(payments, 1, "retry records no duplicate payment row");

    // (c) The invoice payment_status reflects the successful charge.
    let status: i32 = sqlx::query_scalar("SELECT payment_status FROM invoices WHERE id = $1")
        .bind(invoice.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        status,
        PaymentStatus::Succeeded.as_i32(),
        "invoice marked succeeded"
    );
}

/// A FAILED charge is legitimately retried (dunning): the retry reaches the
/// provider again and the payments row is updated in place with the new
/// provider payment id + outcome (one row, final status `succeeded`).
#[sqlx::test(migrations = "../rago-db/migrations")]
async fn failed_charge_retries_and_records_new_outcome(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let customer = Uuid::new_v4();

    let invoice = finalized_invoice(org, customer);
    db.insert_invoice(&invoice).await.unwrap();

    // First attempt: card declined. Second attempt: succeeds with a NEW intent.
    let transport = MockTransport::scripted(
        vec![Ok(rago_integrations::HttpResponse {
            status: 200,
            body: r#"{"id":"pi_fail_1","status":"requires_payment_method"}"#.into(),
        })],
        rago_integrations::HttpResponse {
            status: 200,
            body: r#"{"id":"pi_retry_2","status":"succeeded"}"#.into(),
        },
    );

    let first = charge_invoice(&db, &transport, "sk_test_x", &invoice, "cus_1")
        .await
        .unwrap();
    assert_eq!(first, PayableStatus::Failed);

    let second = charge_invoice(&db, &transport, "sk_test_x", &invoice, "cus_1")
        .await
        .unwrap();
    assert_eq!(second, PayableStatus::Succeeded, "failed charge may be retried");
    assert_eq!(transport.requests().len(), 2, "retry of a FAILED charge reaches the provider");

    // Still exactly one payments row, now reflecting the retry's outcome so an
    // incoming provider webhook for `pi_retry_2` can match the payment.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT provider_payment_id, status FROM payments WHERE invoice_id = $1",
    )
    .bind(invoice.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "retry updates the row in place");
    assert_eq!(rows[0].0, "pi_retry_2", "new provider payment id recorded");
    assert_eq!(rows[0].1, "succeeded", "final status recorded");

    // A third run is short-circuited: the invoice is paid, no provider call.
    let third = charge_invoice(&db, &transport, "sk_test_x", &invoice, "cus_1")
        .await
        .unwrap();
    assert_eq!(third, PayableStatus::Succeeded);
    assert_eq!(transport.requests().len(), 2, "paid invoice is never re-charged");
}

/// Distinct invoices must not share a key (a collision would wrongly dedupe a
/// legitimate second charge).
#[sqlx::test(migrations = "../rago-db/migrations")]
async fn distinct_invoices_use_distinct_keys_and_rows(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let customer = Uuid::new_v4();

    let invoice_a = finalized_invoice(org, customer);
    let invoice_b = finalized_invoice(org, customer);
    db.insert_invoice(&invoice_a).await.unwrap();
    db.insert_invoice(&invoice_b).await.unwrap();

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    charge_invoice(&db, &transport, "sk_test_x", &invoice_a, "cus_1")
        .await
        .unwrap();
    charge_invoice(&db, &transport, "sk_test_x", &invoice_b, "cus_1")
        .await
        .unwrap();

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    let key_a = idempotency_key(&requests[0].headers).unwrap();
    let key_b = idempotency_key(&requests[1].headers).unwrap();
    assert_ne!(key_a, key_b, "different invoices derive different keys");

    let payments: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(payments, 2, "one payment row per invoice");
}

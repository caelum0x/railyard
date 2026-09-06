// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Dunning engine (automated failed-payment recovery), through the real
//! `process_dunning_campaigns` sweep against a migrated Postgres.
//!
//! Money-first proofs: threshold firing + payment-request creation/links,
//! net-amount math (credit-note offset), the `days_between_attempts` gate
//! (idempotent re-run, no double request / no double charge), `max_attempts`
//! stop with a single `dunning_campaign.finished`, per-currency isolation, and
//! org-scoping.

use chrono::{DateTime, TimeZone, Utc};
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Customer, Invoice};
use rago_db::Db;
use rago_integrations::MockTransport;
use rago_jobs::dunning::{process_dunning_campaigns, DunningConfig};
use sqlx::PgPool;
use uuid::Uuid;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap()
}

fn customer(org: Uuid, external: &str, currency: &str) -> Customer {
    let ts = Utc::now();
    Customer {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        external_id: external.into(),
        name: Some("Acme".into()),
        email: Some("billing@acme.example".into()),
        country: Some("US".into()),
        currency: Some(currency.into()),
        timezone: None,
        tax_identification_number: None,
        created_at: ts,
        updated_at: ts,
        deleted_at: None,
    }
}

fn invoice(org: Uuid, cust: Uuid, currency: &str, total: i64) -> Invoice {
    let ts = Utc::now();
    Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(cust),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.into()),
        issuing_date: None,
        fees_amount_cents: total,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: total,
        sub_total_including_taxes_amount_cents: total,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: total,
        version_number: 4,
        created_at: ts,
        updated_at: ts,
        applied_taxes: Vec::new(),
    }
}

/// Insert a finalized, payment-overdue invoice; returns its id.
async fn overdue_invoice(db: &Db, org: Uuid, cust: Uuid, currency: &str, total: i64) -> Uuid {
    let inv = invoice(org, cust, currency, total);
    db.insert_invoice(&inv).await.unwrap();
    sqlx::query("UPDATE invoices SET payment_overdue = true WHERE id = $1")
        .bind(inv.id)
        .execute(&db.pool)
        .await
        .unwrap();
    inv.id
}

/// Insert a campaign (applied to the org as the default) + one USD threshold.
async fn campaign_with_threshold(
    db: &Db,
    org: Uuid,
    max_attempts: i32,
    days_between: i32,
    currency: &str,
    threshold_cents: i64,
) -> Uuid {
    let campaign_id = Uuid::new_v4();
    db.insert_dunning_campaign(
        campaign_id,
        org,
        "Default",
        "default",
        None,
        max_attempts,
        days_between,
        &serde_json::json!([]),
        true, // applied_to_organization → org default
    )
    .await
    .unwrap();
    db.upsert_dunning_campaign_threshold(
        Uuid::new_v4(),
        org,
        campaign_id,
        currency,
        threshold_cents,
    )
    .await
    .unwrap();
    campaign_id
}

fn charging_cfg() -> DunningConfig {
    DunningConfig {
        stripe_secret_key: "sk_test_x".into(),
        provider: "stripe".into(),
        provider_customer_fallback: Some("cus_fallback".into()),
        default_webhook_endpoint: None,
    }
}

async fn payment_request_count(db: &Db, org: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM payment_requests WHERE organization_id = $1")
        .bind(org)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn webhook_count(db: &Db, org: Uuid, webhook_type: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhooks WHERE organization_id = $1 AND webhook_type = $2",
    )
    .bind(org)
    .bind(webhook_type)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn fires_threshold_creates_request_links_and_charges(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    db.insert_webhook_endpoint(org, "https://hooks.example/rago")
        .await
        .unwrap();
    let cust = customer(org, "acme", "USD");
    db.insert_customer(&cust).await.unwrap();
    campaign_with_threshold(&db, org, 3, 7, "USD", 5_000).await;
    let inv = overdue_invoice(&db, org, cust.id, "USD", 9_000).await;

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    let report = process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();

    assert_eq!(report.customers_evaluated, 1);
    assert_eq!(report.payment_requests_created, 1);
    assert_eq!(report.invoices_charged, 1);

    // One payment request, linked to the overdue invoice, priced at the full
    // overdue total (no credits here).
    assert_eq!(payment_request_count(&db, org).await, 1);
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_cents FROM payment_requests WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(amount, 9_000);
    let links: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payment_request_applied_invoices WHERE invoice_id = $1",
    )
    .bind(inv)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(links, 1);

    // Exactly one payments row (deterministic id) and the invoice is charged.
    let payments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE invoice_id = $1")
        .bind(inv)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(payments, 1);

    // The `payment_request.created` webhook fired; attempts advanced to 1.
    assert_eq!(webhook_count(&db, org, "payment_request.created").await, 1);
    let (attempts, last) = db.customer_dunning_attempts(cust.id).await.unwrap();
    assert_eq!(attempts.get("USD"), Some(&1));
    assert!(last.is_some());
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn net_amount_subtracts_credit_note_offset(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let cust = customer(org, "acme", "USD");
    db.insert_customer(&cust).await.unwrap();
    campaign_with_threshold(&db, org, 3, 7, "USD", 5_000).await;
    let inv = overdue_invoice(&db, org, cust.id, "USD", 10_000).await;

    // A finalized credit note offsets 3_000 of the invoice.
    let cn = rago_core::models::CreditNote {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: cust.id,
        invoice_id: inv,
        sequential_id: 1,
        number: "CN-1".into(),
        issuing_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        reason: 0,
        credit_status: Some(0),
        refund_status: None,
        status: 1, // finalized
        credit_amount_cents: 3_000,
        credit_amount_currency: "USD".into(),
        balance_amount_cents: 3_000,
        refund_amount_cents: 0,
        total_amount_cents: 3_000,
        total_amount_currency: "USD".into(),
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        coupons_adjustment_amount_cents: 0,
        precise_coupons_adjustment_amount_cents: rust_decimal::Decimal::ZERO,
        precise_taxes_amount_cents: rust_decimal::Decimal::ZERO,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        voided_at: None,
        applied_taxes: Vec::new(),
        items: Vec::new(),
    };
    db.insert_credit_note(&cn).await.unwrap();

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();

    // Threshold evaluation used the RAW overdue balance (10_000 ≥ 5_000 → fire),
    // but the payment request nets out the credit-note offset: 10_000 − 3_000.
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_cents FROM payment_requests WHERE organization_id = $1")
            .bind(org)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(amount, 7_000);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn days_between_gate_prevents_double_request_and_double_charge(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let cust = customer(org, "acme", "USD");
    db.insert_customer(&cust).await.unwrap();
    campaign_with_threshold(&db, org, 3, 7, "USD", 5_000).await;
    let inv = overdue_invoice(&db, org, cust.id, "USD", 9_000).await;

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    // Two sweeps at the SAME instant — the second is inside the days-between
    // window and must be gated.
    process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();
    let second = process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();

    assert_eq!(second.payment_requests_created, 0, "second sweep is gated");
    assert_eq!(
        payment_request_count(&db, org).await,
        1,
        "no duplicate request"
    );
    let payments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE invoice_id = $1")
        .bind(inv)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(payments, 1, "no double charge (deterministic payment id)");
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn max_attempts_stop_emits_single_finished(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    db.insert_webhook_endpoint(org, "https://hooks.example/rago")
        .await
        .unwrap();
    let cust = customer(org, "acme", "USD");
    db.insert_customer(&cust).await.unwrap();
    // max_attempts = 1 → a single attempt exhausts the currency.
    campaign_with_threshold(&db, org, 1, 7, "USD", 5_000).await;
    overdue_invoice(&db, org, cust.id, "USD", 9_000).await;

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    let first = process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();
    assert_eq!(first.campaigns_finished, 1);
    assert_eq!(
        webhook_count(&db, org, "dunning_campaign.finished").await,
        1
    );

    // A later sweep (past the window) must NOT re-dun: the currency is already
    // at max_attempts → no new request, no second finished webhook.
    let later = now() + chrono::Duration::days(30);
    let second = process_dunning_campaigns(&db, &transport, &charging_cfg(), later)
        .await
        .unwrap();
    assert_eq!(second.payment_requests_created, 0);
    assert_eq!(second.campaigns_finished, 0);
    assert_eq!(payment_request_count(&db, org).await, 1);
    assert_eq!(
        webhook_count(&db, org, "dunning_campaign.finished").await,
        1
    );
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn per_currency_isolation(pool: PgPool) {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let cust = customer(org, "acme", "USD");
    db.insert_customer(&cust).await.unwrap();
    let campaign_id = campaign_with_threshold(&db, org, 3, 7, "USD", 5_000).await;
    // Add a EUR threshold at 10_000.
    db.upsert_dunning_campaign_threshold(Uuid::new_v4(), org, campaign_id, "EUR", 10_000)
        .await
        .unwrap();
    // USD overdue clears its threshold; EUR overdue does not.
    overdue_invoice(&db, org, cust.id, "USD", 6_000).await;
    overdue_invoice(&db, org, cust.id, "EUR", 9_000).await;

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();

    // Exactly one request, in USD.
    assert_eq!(payment_request_count(&db, org).await, 1);
    let currency: String = sqlx::query_scalar(
        "SELECT amount_currency FROM payment_requests WHERE organization_id = $1",
    )
    .bind(org)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(currency, "USD");
    let (attempts, _) = db.customer_dunning_attempts(cust.id).await.unwrap();
    assert_eq!(attempts.get("USD"), Some(&1));
    assert_eq!(attempts.get("EUR"), None);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn org_scoping_isolates_customers(pool: PgPool) {
    let db = Db::new(pool);
    let org_a = Uuid::new_v4();
    let org_b = Uuid::new_v4();
    let cust_a = customer(org_a, "a", "USD");
    let cust_b = customer(org_b, "b", "USD");
    db.insert_customer(&cust_a).await.unwrap();
    db.insert_customer(&cust_b).await.unwrap();
    // Only org A has a campaign; both have overdue invoices.
    campaign_with_threshold(&db, org_a, 3, 7, "USD", 5_000).await;
    overdue_invoice(&db, org_a, cust_a.id, "USD", 9_000).await;
    overdue_invoice(&db, org_b, cust_b.id, "USD", 9_000).await;

    let transport = MockTransport::always(200, r#"{"id":"pi_1","status":"succeeded"}"#);
    process_dunning_campaigns(&db, &transport, &charging_cfg(), now())
        .await
        .unwrap();

    assert_eq!(payment_request_count(&db, org_a).await, 1, "org A dunned");
    assert_eq!(
        payment_request_count(&db, org_b).await,
        0,
        "org B has no campaign → not dunned"
    );
    // org B's customer attempts are untouched.
    let (attempts_b, _) = db.customer_dunning_attempts(cust_b.id).await.unwrap();
    assert!(attempts_b.is_empty());
}

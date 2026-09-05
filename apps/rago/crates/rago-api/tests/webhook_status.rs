// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Monotonic payment-status sync for incoming provider webhooks.
//!
//! Provider events can arrive late or be replayed. A delayed
//! `payment_intent.payment_failed` after `payment_intent.succeeded` must never
//! flip a paid invoice back to failed — terminal success (and a refund) is
//! never downgraded by a later event.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use rago_integrations::payments::stripe_webhook::signature_header;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "whsec_rago_test";

// ---- seed helpers ----------------------------------------------------------

fn org() -> m::Organization {
    let now = Utc::now();
    m::Organization {
        id: Uuid::new_v4(),
        name: "Acme".into(),
        slug: format!("acme-{}", &Uuid::new_v4().to_string()[..8]),
        default_currency: "USD".into(),
        email: Some("ops@acme.test".into()),
        country: Some("US".into()),
        tax_identification_number: None,
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: true,
        hmac_key: Some(Uuid::new_v4().to_string()),
        created_at: now,
        updated_at: now,
    }
}

fn customer(org_id: Uuid) -> m::Customer {
    let now = Utc::now();
    m::Customer {
        id: Uuid::new_v4(),
        organization_id: org_id,
        billing_entity_id: org_id,
        external_id: "cust-1".into(),
        name: Some("Customer".into()),
        email: Some("c@acme.test".into()),
        country: Some("US".into()),
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn invoice(org_id: Uuid, customer_id: Uuid) -> m::Invoice {
    let now = Utc::now();
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: org_id,
        billing_entity_id: org_id,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::OneOff,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: Some(now.date_naive()),
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

/// Seed org + customer + invoice + a pending payment with the given provider
/// payment id; return the router, db, org id, and invoice id.
async fn setup(pool: PgPool, provider_payment_id: &str) -> (axum::Router, Db, Uuid, Uuid) {
    std::env::set_var("RAGO_STRIPE_WEBHOOK_SECRET", WEBHOOK_SECRET);
    let db = Db::new(pool);
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    let cust = customer(organization.id);
    db.insert_customer(&cust).await.unwrap();
    let inv = invoice(organization.id, cust.id);
    db.insert_invoice(&inv).await.unwrap();
    db.insert_payment(
        Uuid::new_v4(),
        organization.id,
        inv.id,
        "stripe",
        Some(provider_payment_id),
        Some("processing"),
        "pending",
        1000,
        "USD",
    )
    .await
    .unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    (app, db, organization.id, inv.id)
}

/// POST a signed Stripe event to `/webhooks/stripe/:org`.
async fn post_stripe_event(app: &axum::Router, org_id: Uuid, event_type: &str, pi: &str) -> StatusCode {
    let payload = json!({ "type": event_type, "data": { "object": { "id": pi } } }).to_string();
    let sig = signature_header(WEBHOOK_SECRET, Utc::now().timestamp(), &payload);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/webhooks/stripe/{org_id}"))
        .header("Stripe-Signature", sig)
        .body(Body::from(payload))
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

async fn payment_status(db: &Db, invoice_id: Uuid) -> String {
    db.list_payments(invoice_id).await.unwrap()[0].2.clone()
}

async fn invoice_payment_status(db: &Db, invoice_id: Uuid) -> PaymentStatus {
    db.find_invoice(invoice_id).await.unwrap().unwrap().payment_status
}

// ---- monotonic-status guard ------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn delayed_failed_event_never_downgrades_a_paid_invoice(pool: PgPool) {
    let (app, db, org_id, inv_id) = setup(pool, "pi_replay").await;

    // Settlement succeeds.
    let status = post_stripe_event(&app, org_id, "payment_intent.succeeded", "pi_replay").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "succeeded");
    assert_eq!(invoice_payment_status(&db, inv_id).await, PaymentStatus::Succeeded);

    // A delayed/replayed `payment_failed` for the same intent arrives later —
    // the terminal success must not be flipped back to failed.
    let status = post_stripe_event(&app, org_id, "payment_intent.payment_failed", "pi_replay").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "succeeded");
    assert_eq!(invoice_payment_status(&db, inv_id).await, PaymentStatus::Succeeded);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn failed_then_succeeded_still_upgrades(pool: PgPool) {
    let (app, db, org_id, inv_id) = setup(pool, "pi_retry").await;

    // A decline, then a successful retry: upgrading failed → succeeded is fine.
    let status = post_stripe_event(&app, org_id, "payment_intent.payment_failed", "pi_retry").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "failed");

    let status = post_stripe_event(&app, org_id, "payment_intent.succeeded", "pi_retry").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "succeeded");
    assert_eq!(invoice_payment_status(&db, inv_id).await, PaymentStatus::Succeeded);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn refund_after_success_is_allowed_but_not_reversed(pool: PgPool) {
    let (app, db, org_id, inv_id) = setup(pool, "pi_refund").await;

    let status = post_stripe_event(&app, org_id, "payment_intent.succeeded", "pi_refund").await;
    assert_eq!(status, StatusCode::OK);

    // A refund legitimately follows a successful capture.
    let status = post_stripe_event(&app, org_id, "charge.refunded", "pi_refund").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "refunded");
    assert_eq!(invoice_payment_status(&db, inv_id).await, PaymentStatus::Refunded);

    // But a replayed success (or failure) after the refund must not undo it.
    let status = post_stripe_event(&app, org_id, "payment_intent.succeeded", "pi_refund").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "refunded");
    let status = post_stripe_event(&app, org_id, "payment_intent.payment_failed", "pi_refund").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment_status(&db, inv_id).await, "refunded");
    assert_eq!(invoice_payment_status(&db, inv_id).await, PaymentStatus::Refunded);
}

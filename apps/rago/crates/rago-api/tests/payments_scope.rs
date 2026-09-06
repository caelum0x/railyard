// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Cross-tenant isolation for the REST payment surface.
//!
//! Every payment/payment-request/payment-receipt endpoint must be scoped to
//! the authenticated organization: a second org's API key must never be able
//! to read or mutate org A's invoices, payments, payment requests, or
//! receipts. Cross-tenant ids answer `404` (not `403`) so existence does not
//! leak.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN_A: &str = "org-a-api-key";
const TOKEN_B: &str = "org-b-api-key";

// ---- seed helpers ----------------------------------------------------------

fn org(name: &str) -> m::Organization {
    let now = Utc::now();
    m::Organization {
        id: Uuid::new_v4(),
        name: name.into(),
        slug: format!("{name}-{}", &Uuid::new_v4().to_string()[..8]),
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

fn api_key(org_id: Uuid, value: &str) -> m::ApiKey {
    let now = Utc::now();
    m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org_id,
        value: value.into(),
        name: Some("test".into()),
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn customer(org_id: Uuid, external_id: &str) -> m::Customer {
    let now = Utc::now();
    m::Customer {
        id: Uuid::new_v4(),
        organization_id: org_id,
        billing_entity_id: org_id,
        external_id: external_id.into(),
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

// ---- request helper --------------------------------------------------------

async fn call(
    app: &axum::Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let body = match body {
        Some(b) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Seed two orgs (each with an api key), org A's customer + invoice, and
/// return the router + db + both org models + org A's invoice.
async fn setup(
    pool: PgPool,
) -> (
    axum::Router,
    Db,
    m::Organization,
    m::Organization,
    m::Invoice,
) {
    let db = Db::new(pool);
    let org_a = org("acme");
    let org_b = org("rival");
    db.insert_organization(&org_a).await.unwrap();
    db.insert_organization(&org_b).await.unwrap();
    db.insert_api_key(&api_key(org_a.id, TOKEN_A))
        .await
        .unwrap();
    db.insert_api_key(&api_key(org_b.id, TOKEN_B))
        .await
        .unwrap();
    let cust = customer(org_a.id, "cust-a");
    db.insert_customer(&cust).await.unwrap();
    let inv = invoice(org_a.id, cust.id);
    db.insert_invoice(&inv).await.unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    (app, db, org_a, org_b, inv)
}

// ---- cross-tenant isolation ------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_payment_rejects_cross_org_invoice(pool: PgPool) {
    let (app, db, _a, _b, inv) = setup(pool).await;
    let body = json!({ "payment": { "invoice_id": inv.id, "amount_cents": 1000, "amount_currency": "USD" } });

    // org B cannot record a payment against org A's invoice — 404, no row.
    let (status, _) = call(
        &app,
        TOKEN_B,
        "POST",
        "/api/v1/payments",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(db.list_payments(inv.id).await.unwrap().len(), 0);

    // org A (owner) still can.
    let (status, resp) = call(&app, TOKEN_A, "POST", "/api/v1/payments", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{resp:?}");
    assert_eq!(db.list_payments(inv.id).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn list_and_show_payment_reject_cross_org_invoice(pool: PgPool) {
    let (app, _db, _a, _b, inv) = setup(pool).await;
    let body = json!({ "payment": { "invoice_id": inv.id, "amount_cents": 1000, "amount_currency": "USD" } });
    let (status, resp) = call(&app, TOKEN_A, "POST", "/api/v1/payments", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{resp:?}");
    let payment_id = resp["payment"]["lago_id"].as_str().unwrap().to_string();

    // index: org B gets 404 for org A's invoice id (not an empty/filled list).
    let uri = format!("/api/v1/payments?invoice_id={}", inv.id);
    let (status, _) = call(&app, TOKEN_B, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, resp) = call(&app, TOKEN_A, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["payments"].as_array().unwrap().len(), 1);

    // show: org B gets 404 for org A's payment.
    let uri = format!("/api/v1/payments/{payment_id}?invoice_id={}", inv.id);
    let (status, _) = call(&app, TOKEN_B, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, TOKEN_A, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn payment_receipts_reject_cross_org_invoice(pool: PgPool) {
    let (app, _db, _a, _b, inv) = setup(pool).await;
    let body = json!({ "payment": { "invoice_id": inv.id, "amount_cents": 1000, "amount_currency": "USD" } });
    let (status, _) = call(&app, TOKEN_A, "POST", "/api/v1/payments", Some(body)).await;
    assert_eq!(status, StatusCode::OK);

    // receipts index: org B gets 404 for org A's invoice id.
    let uri = format!("/api/v1/payment_receipts?invoice_id={}", inv.id);
    let (status, _) = call(&app, TOKEN_B, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, resp) = call(&app, TOKEN_A, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["payment_receipts"].as_array().unwrap().len(), 1);

    // resend: org B cannot trigger mail for org A's invoice.
    let uri = format!("/api/v1/payment_receipts/{}/resend_email", inv.id);
    let (status, _) = call(&app, TOKEN_B, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, resp) = call(&app, TOKEN_A, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["payment_receipt"]["email_resent"], true);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn show_payment_request_rejects_cross_org_id(pool: PgPool) {
    let (app, db, _a, _b, inv) = setup(pool).await;
    // A payment request collects the customer's OVERDUE invoices (Lago computes
    // the amount from them; it is not caller-supplied) — flag org A's 1000-cent
    // invoice overdue so it is collectible.
    sqlx::query("UPDATE invoices SET payment_overdue = true WHERE id = $1")
        .bind(inv.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let body = json!({ "payment_request": { "external_customer_id": "cust-a" } });
    let (status, resp) = call(
        &app,
        TOKEN_A,
        "POST",
        "/api/v1/payment_requests",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp:?}");
    let request_id = resp["payment_request"]["lago_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Amount is the overdue invoice total, net of credits.
    assert_eq!(resp["payment_request"]["amount_cents"], 1000);

    let uri = format!("/api/v1/payment_requests/{request_id}");
    let (status, _) = call(&app, TOKEN_B, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, resp) = call(&app, TOKEN_A, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["payment_request"]["amount_cents"], 1000);
}

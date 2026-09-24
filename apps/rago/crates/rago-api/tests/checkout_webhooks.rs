// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Hosted-checkout provider webhooks (Creem, Dodo Payments, Iyzico): signed
//! deliveries settle the invoice payment created under the checkout id, move
//! it to the provider's settled payment id, and replays are not re-applied.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use chrono::Utc;
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const CREEM_SECRET: &str = "creem_whsec_test";
const DODO_KEY: &[u8] = b"dodo-test-signing-key";
const IYZICO_SECRET: &str = "sandbox-iyzico-secret";

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


fn env() {
    std::env::set_var("RAGO_CREEM_WEBHOOK_SECRET", CREEM_SECRET);
    std::env::set_var(
        "RAGO_DODO_WEBHOOK_SECRET",
        format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(DODO_KEY)),
    );
    std::env::set_var("RAGO_IYZICO_SECRET_KEY", IYZICO_SECRET);
}

async fn setup(pool: PgPool, provider: &str, reference: &str) -> (axum::Router, Db, Uuid, Uuid) {
    env();
    let db = Db::new(pool);
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    let cust = customer(organization.id);
    db.insert_customer(&cust).await.unwrap();
    let inv = invoice(organization.id, cust.id);
    db.insert_invoice(&inv).await.unwrap();
    db.insert_payment(
        rago_jobs::payments::deterministic_payment_id(inv.id),
        organization.id,
        inv.id,
        provider,
        Some(reference),
        Some("pending"),
        "pending",
        1000,
        "USD",
    )
    .await
    .unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    (app, db, organization.id, inv.id)
}

async fn post(app: &axum::Router, uri: String, headers: &[(&str, String)], body: String) -> (StatusCode, String) {
    let mut req = Request::builder().method("POST").uri(uri);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn payment(db: &Db, invoice_id: Uuid) -> (String, Option<String>) {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status, provider_payment_id FROM payments WHERE invoice_id = $1")
            .bind(invoice_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    row
}

async fn invoice_status(db: &Db, invoice_id: Uuid) -> PaymentStatus {
    db.find_invoice(invoice_id).await.unwrap().unwrap().payment_status
}

fn creem_sig(body: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(CREEM_SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn creem_checkout_completed_settles_once(pool: PgPool) {
    let (app, db, org_id, inv) = setup(pool, "creem", "ch_1").await;
    let body = json!({"id":"evt_1","eventType":"checkout.completed","object":{"id":"ch_1","status":"completed","order":{"id":"ord_1","status":"paid"}}}).to_string();
    let (status, _) = post(&app, format!("/webhooks/creem/{org_id}"), &[("creem-signature", creem_sig(&body))], body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment(&db, inv).await.0, "succeeded");
    assert_eq!(invoice_status(&db, inv).await, PaymentStatus::Succeeded);

    let (status, text) = post(&app, format!("/webhooks/creem/{org_id}"), &[("creem-signature", creem_sig(&body))], body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "duplicate delivery");

    let (status, _) = post(&app, format!("/webhooks/creem/{org_id}"), &[("creem-signature", "00".into())], body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn dodo_payment_succeeded_rebinds_session_to_payment_id(pool: PgPool) {
    let (app, db, org_id, inv) = setup(pool, "dodo", "cks_1").await;
    let body = json!({"business_id":"bus","type":"payment.succeeded","timestamp":"2026-09-24T00:00:00Z","data":{"payload_type":"Payment","payment_id":"pay_1","checkout_session_id":"cks_1","status":"succeeded","metadata":{"lago_invoice_id": inv.to_string()}}}).to_string();
    let ts = Utc::now().timestamp().to_string();
    let sig = rago_integrations::payments::provider_webhook::standard_webhooks_signature(DODO_KEY, "msg_1", &ts, &body);
    let headers = [("webhook-id", "msg_1".to_string()), ("webhook-timestamp", ts.clone()), ("webhook-signature", format!("v1,{sig}"))];
    let (status, _) = post(&app, format!("/webhooks/dodo/{org_id}"), &headers, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment(&db, inv).await, ("succeeded".to_string(), Some("pay_1".to_string())));

    // A stale delivery is rejected before anything is applied.
    let old = (Utc::now().timestamp() - 3_600).to_string();
    let sig = rago_integrations::payments::provider_webhook::standard_webhooks_signature(DODO_KEY, "msg_2", &old, &body);
    let headers = [("webhook-id", "msg_2".to_string()), ("webhook-timestamp", old), ("webhook-signature", format!("v1,{sig}"))];
    let (status, _) = post(&app, format!("/webhooks/dodo/{org_id}"), &headers, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn iyzico_notification_binds_token_to_payment_id(pool: PgPool) {
    let (app, db, org_id, inv) = setup(pool, "iyzico", "tok_1").await;
    let sig = rago_integrations::payments::provider_webhook::iyzico_notification_signature(
        IYZICO_SECRET, "CHECKOUT_FORM_AUTH", "123456", Some("tok_1"), "conv", "SUCCESS",
    );
    let body = json!({"iyziEventType":"CHECKOUT_FORM_AUTH","paymentId":123456,"token":"tok_1","paymentConversationId":"conv","status":"SUCCESS"}).to_string();
    let (status, _) = post(&app, format!("/webhooks/iyzico/{org_id}"), &[("x-iyz-signature-v3", sig)], body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment(&db, inv).await, ("succeeded".to_string(), Some("123456".to_string())));
    assert_eq!(invoice_status(&db, inv).await, PaymentStatus::Succeeded);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn another_organization_cannot_settle_the_payment(pool: PgPool) {
    let (app, db, _org_id, inv) = setup(pool, "creem", "ch_9").await;
    let other = org();
    db.insert_organization(&other).await.unwrap();
    let body = json!({"id":"evt_9","eventType":"checkout.completed","object":{"id":"ch_9","status":"completed"}}).to_string();
    let (status, text) = post(&app, format!("/webhooks/creem/{}", other.id), &[("creem-signature", creem_sig(&body))], body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "payment not found");
    assert_eq!(payment(&db, inv).await.0, "pending");
}

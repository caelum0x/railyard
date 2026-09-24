// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Hosted customer portal: signed expiring links, revocation, and strict
//! organization / customer isolation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

struct World {
    app: axum::Router,
    db: Db,
    org: m::Organization,
    customer: m::Customer,
    invoice: m::Invoice,
}

async fn world(pool: PgPool) -> World {
    let db = Db::new(pool);
    let org = org();
    db.insert_organization(&org).await.unwrap();
    db.insert_api_key(&api_key(org.id, "key-a")).await.unwrap();
    let customer = customer(org.id, "cust-a");
    db.insert_customer(&customer).await.unwrap();
    let invoice = invoice(org.id, customer.id);
    db.insert_invoice(&invoice).await.unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    World { app, db, org, customer, invoice }
}

async fn send(app: &axum::Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        b = b.header("authorization", format!("Bearer {k}"));
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap().to_vec();
    (status, headers, bytes)
}

async fn issue_via_api(w: &World) -> String {
    let (status, _, body) = send(
        &w.app,
        "POST",
        "/api/v1/customers/cust-a/portal_sessions",
        Some("key-a"),
        Some(json!({ "portal_session": { "expires_in_minutes": 60 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    let url = v["portal_session"]["portal_url"].as_str().unwrap().to_string();
    assert!(url.contains("/portal/"));
    v["portal_session"]["token"].as_str().unwrap().to_string()
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn issued_link_renders_the_customers_invoices_privately(pool: PgPool) {
    let w = world(pool).await;
    let token = issue_via_api(&w).await;
    let (status, headers, body) = send(&w.app, "GET", &format!("/portal/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains(&w.invoice.number));
    assert!(html.contains("10.00 USD"));
    assert!(html.contains("Acme"));
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");

    let (status, headers, pdf) =
        send(&w.app, "GET", &format!("/portal/{token}/invoices/{}/download", w.invoice.id), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "application/pdf");
    assert!(pdf.starts_with(b"%PDF"));
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn another_customers_invoice_is_not_reachable(pool: PgPool) {
    let w = world(pool).await;
    let other = customer(w.org.id, "cust-b");
    w.db.insert_customer(&other).await.unwrap();
    let theirs = invoice(w.org.id, other.id);
    w.db.insert_invoice(&theirs).await.unwrap();
    let token = issue_via_api(&w).await;

    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{token}/invoices/{}/download", theirs.id), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, body) = send(&w.app, "GET", &format!("/portal/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!String::from_utf8(body).unwrap().contains(&theirs.number));
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn expired_revoked_and_foreign_org_links_are_refused(pool: PgPool) {
    let w = world(pool).await;

    // Expired: a correctly signed token whose session has lapsed.
    let key = rago_api::portal::token::derive_key(w.org.hmac_key.as_deref().unwrap());
    let sid = Uuid::new_v4();
    let past = Utc::now() - Duration::minutes(5);
    w.db.insert_portal_session(sid, w.org.id, w.customer.id, past).await.unwrap();
    let expired = rago_api::portal::token::sign(
        &rago_api::portal::token::PortalClaims {
            session_id: sid,
            organization_id: w.org.id,
            customer_id: w.customer.id,
            expires_at: past.timestamp(),
        },
        &key,
    );
    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{expired}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Revoked: a valid link stops working after DELETE.
    let token = issue_via_api(&w).await;
    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = send(&w.app, "DELETE", "/api/v1/customers/cust-a/portal_sessions", Some("key-a"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["portal_sessions"]["revoked"], 1);
    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{token}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Another organization: its key cannot mint a link for our customer, and
    // its API key cannot issue one either.
    let org_b = org();
    w.db.insert_organization(&org_b).await.unwrap();
    w.db.insert_api_key(&api_key(org_b.id, "key-b")).await.unwrap();
    let sid = Uuid::new_v4();
    w.db.insert_portal_session(sid, w.org.id, w.customer.id, Utc::now() + Duration::hours(1)).await.unwrap();
    let forged = rago_api::portal::token::sign(
        &rago_api::portal::token::PortalClaims {
            session_id: sid,
            organization_id: w.org.id,
            customer_id: w.customer.id,
            expires_at: (Utc::now() + Duration::hours(1)).timestamp(),
        },
        &rago_api::portal::token::derive_key(org_b.hmac_key.as_deref().unwrap()),
    );
    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{forged}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(&w.app, "POST", "/api/v1/customers/cust-a/portal_sessions", Some("key-b"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn portal_url_falls_back_to_the_rago_portal_without_stripe(pool: PgPool) {
    let w = world(pool).await;
    std::env::remove_var("RAGO_STRIPE_SECRET_KEY");
    std::env::remove_var("RAGO_STRIPE_SECRET");
    std::env::remove_var("RAGO_PAYMENT_PROVIDER");
    let (status, _, body) = send(&w.app, "GET", "/api/v1/customers/cust-a/portal_url", Some("key-a"), None).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    let url = v["customer"]["portal_url"].as_str().unwrap();
    let token = url.rsplit("/portal/").next().unwrap();
    let (status, _, _) = send(&w.app, "GET", &format!("/portal/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn actions_on_foreign_records_are_refused(pool: PgPool) {
    let w = world(pool).await;
    let token = issue_via_api(&w).await;
    // Unknown wallet / subscription: no side effects, a notice redirect.
    let (status, headers, _) = send(&w.app, "POST", &format!("/portal/{token}/wallets/{}/top_up", Uuid::new_v4()), None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(headers.get("location").unwrap().to_str().unwrap().contains("notice=top_up_invalid"));
    let (status, headers, _) = send(&w.app, "POST", &format!("/portal/{token}/subscriptions/{}/cancel", Uuid::new_v4()), None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(headers.get("location").unwrap().to_str().unwrap().contains("notice=subscription_unavailable"));
    // Paying another customer's invoice is a 404.
    let other = customer(w.org.id, "cust-c");
    w.db.insert_customer(&other).await.unwrap();
    let theirs = invoice(w.org.id, other.id);
    w.db.insert_invoice(&theirs).await.unwrap();
    let (status, _, _) = send(&w.app, "POST", &format!("/portal/{token}/invoices/{}/pay", theirs.id), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

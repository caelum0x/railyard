// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Live-DB integration tests for the REST v1/v2 CRUD tail.
//!
//! Each test runs on an isolated, migrated Postgres via
//! `#[sqlx::test(migrator = "rago_db::MIGRATOR")]` (DB-less CI skips these —
//! sqlx requires `DATABASE_URL`). Every case drives a real axum request through
//! `rago_api::router` with a Bearer api-key, then asserts both the returned
//! Lago-shaped JSON and the persisted DB change.

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

const TOKEN: &str = "test-api-key-value";

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

fn api_key(org_id: Uuid) -> m::ApiKey {
    let now = Utc::now();
    m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org_id,
        value: TOKEN.into(),
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
        name: Some("Original".into()),
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

fn plan(org_id: Uuid, code: &str) -> m::Plan {
    let now = Utc::now();
    m::Plan {
        id: Uuid::new_v4(),
        organization_id: org_id,
        name: "Starter".into(),
        code: code.into(),
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

fn metric(org_id: Uuid, code: &str) -> m::BillableMetric {
    let now = Utc::now();
    m::BillableMetric {
        id: Uuid::new_v4(),
        organization_id: org_id,
        name: "API Calls".into(),
        code: code.into(),
        description: None,
        aggregation_type: AggregationType::CountAgg,
        field_name: None,
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

/// Build the app + seed an org and its api-key; returns the router, a `Db` for
/// direct assertions, and the seeded org.
async fn setup(pool: PgPool) -> (axum::Router, Db, m::Organization) {
    let db = Db::new(pool);
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    db.insert_api_key(&api_key(organization.id)).await.unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    (app, db, organization)
}

// ---- request helper --------------------------------------------------------

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
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

// ---- taxes -----------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn tax_crud(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/taxes",
        Some(json!({ "tax": { "name": "VAT", "code": "vat", "rate": 20.0 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tax"]["code"], "vat");
    assert_eq!(body["tax"]["rate"], 20.0);
    assert!(db.find_tax_by_code(org.id, "vat").await.unwrap().is_some());

    let (status, body) = call(&app, "GET", "/api/v1/taxes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["taxes"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/taxes/vat",
        Some(json!({ "tax": { "rate": 18.5 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tax"]["rate"], 18.5);
    assert_eq!(
        db.find_tax_by_code(org.id, "vat")
            .await
            .unwrap()
            .unwrap()
            .rate,
        18.5
    );

    let (status, _) = call(&app, "DELETE", "/api/v1/taxes/vat", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db.find_tax_by_code(org.id, "vat").await.unwrap().is_none());
}

// ---- webhook endpoints -----------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn webhook_endpoint_crud(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/webhook_endpoints",
        Some(json!({ "webhook_endpoint": { "webhook_url": "https://hook.test/a" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["webhook_endpoint"]["lago_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        body["webhook_endpoint"]["webhook_url"],
        "https://hook.test/a"
    );
    assert_eq!(db.list_webhook_endpoints(org.id).await.unwrap().len(), 1);

    let uri = format!("/api/v1/webhook_endpoints/{id}");
    let (status, body) = call(
        &app,
        "PUT",
        &uri,
        Some(json!({ "webhook_endpoint": { "webhook_url": "https://hook.test/b" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["webhook_endpoint"]["webhook_url"],
        "https://hook.test/b"
    );

    let (status, _) = call(&app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(db.list_webhook_endpoints(org.id).await.unwrap().len(), 0);
}

// ---- billing entities + organizations --------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn billing_entity_and_org(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, body) = call(&app, "POST", "/api/v1/billing_entities", Some(json!({ "billing_entity": { "code": "eu", "name": "EU Ltd", "default_currency": "EUR" } }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["billing_entity"]["code"], "eu");
    assert_eq!(db.list_billing_entities(org.id).await.unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/billing_entities/eu",
        Some(json!({ "billing_entity": { "name": "EU GmbH" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["billing_entity"]["name"], "EU GmbH");

    let (status, body) = call(&app, "GET", "/api/v1/organizations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["organization"]["name"], "Acme");

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/organizations",
        Some(json!({ "organization": { "name": "Acme Inc" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["organization"]["name"], "Acme Inc");
}

// ---- plans + charges -------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn plan_update_destroy_and_charges(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let pl = plan(org.id, "starter");
    db.insert_plan(&pl).await.unwrap();
    db.insert_billable_metric(&metric(org.id, "api_calls"))
        .await
        .unwrap();

    // index
    let (status, body) = call(&app, "GET", "/api/v1/plans", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plans"].as_array().unwrap().len(), 1);

    // update
    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/plans/starter",
        Some(json!({ "plan": { "name": "Starter+", "amount_cents": 2500 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan"]["name"], "Starter+");
    assert_eq!(
        db.find_plan_by_code(org.id, "starter")
            .await
            .unwrap()
            .unwrap()
            .amount_cents,
        Some(2500)
    );

    // add a charge
    let (status, body) = call(&app, "POST", "/api/v1/plans/starter/charges", Some(json!({ "charge": { "billable_metric_code": "api_calls", "charge_model": "standard", "properties": { "amount": "0.01" } } }))).await;
    assert_eq!(status, StatusCode::OK);
    let charge_id = body["charge"]["lago_id"].as_str().unwrap().to_string();
    let plan_id = db
        .find_plan_by_code(org.id, "starter")
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(db.charges_for_plan(plan_id).await.unwrap().len(), 1);

    // update the charge
    let uri = format!("/api/v1/plans/starter/charges/{charge_id}");
    let (status, _) = call(
        &app,
        "PUT",
        &uri,
        Some(json!({ "charge": { "min_amount_cents": 500 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // destroy the charge
    let (status, _) = call(&app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(db.charges_for_plan(plan_id).await.unwrap().len(), 0);

    // destroy the plan
    let (status, _) = call(&app, "DELETE", "/api/v1/plans/starter", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_plan_by_code(org.id, "starter")
        .await
        .unwrap()
        .is_none());
}

// ---- billable metrics ------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn billable_metric_crud(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    db.insert_billable_metric(&metric(org.id, "api_calls"))
        .await
        .unwrap();

    let (status, body) = call(&app, "GET", "/api/v1/billable_metrics", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["billable_metrics"].as_array().unwrap().len(), 1);

    let (status, body) = call(&app, "GET", "/api/v1/billable_metrics/api_calls", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["billable_metric"]["code"], "api_calls");

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/billable_metrics/api_calls",
        Some(json!({ "billable_metric": { "name": "Requests" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["billable_metric"]["name"], "Requests");

    let (status, _) = call(&app, "DELETE", "/api/v1/billable_metrics/api_calls", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_metric_by_code(org.id, "api_calls")
        .await
        .unwrap()
        .is_none());
}

// ---- coupons ---------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn coupon_crud(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, _) = call(&app, "POST", "/api/v1/coupons", Some(json!({ "coupon": { "name": "Welcome", "code": "welcome", "coupon_type": "fixed_amount", "amount_cents": 500, "amount_currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_coupon_by_code(org.id, "welcome")
        .await
        .unwrap()
        .is_some());

    let (status, body) = call(&app, "GET", "/api/v1/coupons", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["coupons"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/coupons/welcome",
        Some(json!({ "coupon": { "name": "Welcome!" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["coupon"]["name"], "Welcome!");

    let (status, _) = call(&app, "DELETE", "/api/v1/coupons/welcome", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_coupon_by_code(org.id, "welcome")
        .await
        .unwrap()
        .is_none());
}

// ---- customer destroy + nested ---------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn customer_destroy_and_nested(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let pl = plan(org.id, "starter");
    db.insert_plan(&pl).await.unwrap();
    let sub = m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: c.id,
        plan_id: pl.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-1".into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(Utc::now()),
        started_at: Some(Utc::now()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    db.insert_subscription(&sub).await.unwrap();

    let (status, body) = call(&app, "GET", "/api/v1/customers/cust-1/subscriptions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subscriptions"].as_array().unwrap().len(), 1);

    let (status, body) = call(&app, "GET", "/api/v1/customers/cust-1/portal_url", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["customer"]["portal_url"].is_null());

    let (status, _) = call(&app, "DELETE", "/api/v1/customers/cust-1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_customer_by_external_id(org.id, "cust-1")
        .await
        .unwrap()
        .is_none());
}

// ---- subscription update/show + alerts CRUD --------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn subscription_update_and_alerts(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let pl = plan(org.id, "starter");
    db.insert_plan(&pl).await.unwrap();
    let sub = m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: c.id,
        plan_id: pl.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-1".into(),
        name: Some("Old".into()),
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(Utc::now()),
        started_at: Some(Utc::now()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    db.insert_subscription(&sub).await.unwrap();

    let (status, body) = call(&app, "GET", "/api/v1/subscriptions/sub-1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subscription"]["external_id"], "sub-1");

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/subscriptions/sub-1",
        Some(json!({ "subscription": { "name": "New" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subscription"]["name"], "New");

    // alert create + index + show + update + destroy
    let (status, body) = call(&app, "POST", "/api/v1/subscriptions/sub-1/alerts", Some(json!({ "alert": { "alert_type": "current_usage_amount", "code": "warn", "thresholds": [{ "value": 1000, "recurring": false }] } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["alert"]["code"], "warn");
    assert_eq!(body["alert"]["thresholds"].as_array().unwrap().len(), 1);

    let (status, body) = call(&app, "GET", "/api/v1/subscriptions/sub-1/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alerts"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/subscriptions/sub-1/alerts/warn",
        Some(json!({ "alert": { "name": "Warn80" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alert"]["name"], "Warn80");

    let (status, _) = call(
        &app,
        "DELETE",
        "/api/v1/subscriptions/sub-1/alerts/warn",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&app, "GET", "/api/v1/subscriptions/sub-1/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alerts"].as_array().unwrap().len(), 0);
}

// ---- invoice actions -------------------------------------------------------

fn draft_invoice(org_id: Uuid, customer_id: Uuid) -> m::Invoice {
    let now = Utc::now();
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: org_id,
        billing_entity_id: org_id,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::OneOff,
        status: InvoiceStatus::Draft,
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

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn invoice_actions(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();
    let uri_base = format!("/api/v1/invoices/{}", inv.id);

    // finalize
    let (status, body) = call(&app, "PUT", &format!("{uri_base}/finalize"), None).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["invoice"]["status"], "finalized");
    assert_eq!(
        db.find_invoice(inv.id)
            .await
            .unwrap()
            .unwrap()
            .status
            .as_str(),
        "finalized"
    );

    // refresh (recompute totals)
    let (status, _) = call(&app, "PUT", &format!("{uri_base}/refresh"), None).await;
    assert_eq!(status, StatusCode::OK);

    // void
    let (status, body) = call(&app, "POST", &format!("{uri_base}/void"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["invoice"]["status"], "voided");

    // honest-partial provider actions still resolve
    let (status, body) = call(&app, "POST", &format!("{uri_base}/payment_url"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["invoice_payment_details"]["payment_url"].is_null());
}

// ---- payments + payment requests -------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn payments_and_requests(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();

    let (status, body) = call(&app, "POST", "/api/v1/payments", Some(json!({ "payment": { "invoice_id": inv.id, "amount_cents": 1000, "amount_currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let payment_id = body["payment"]["lago_id"].as_str().unwrap().to_string();
    assert_eq!(db.list_payments(inv.id).await.unwrap().len(), 1);

    let uri = format!("/api/v1/payments?invoice_id={}", inv.id);
    let (status, body) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["payments"].as_array().unwrap().len(), 1);

    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/payments/{payment_id}?invoice_id={}", inv.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A payment request collects the customer's overdue invoices and computes
    // the amount from them (Lago `PaymentRequests::CreateService`). Finalize +
    // flag the 1000-cent invoice overdue so it is collectible.
    sqlx::query("UPDATE invoices SET status = 1, payment_overdue = true WHERE id = $1")
        .bind(inv.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/payment_requests",
        Some(json!({ "payment_request": { "external_customer_id": "cust-1" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["payment_request"]["amount_cents"], 1000);
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/payment_requests?external_customer_id=cust-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["payment_requests"].as_array().unwrap().len(), 1);
}

// ---- v2 catalog ------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn v2_catalog(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    // product
    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/products",
        Some(json!({ "product": { "code": "seats", "name": "Seats" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let product_id = body["product"]["lago_id"].as_str().unwrap().to_string();
    assert_eq!(db.list_products(org.id).await.unwrap().len(), 1);

    // rate card
    let (status, body) = call(&app, "POST", "/api/v2/rate_cards", Some(json!({ "rate_card": { "product_id": product_id, "code": "rc1", "name": "RC1", "currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rate_card_id = body["rate_card"]["lago_id"].as_str().unwrap().to_string();

    // rate
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v2/rate_cards/{rate_card_id}/rates"),
        Some(json!({ "rate": { "charge_model": "standard", "properties": { "amount": "1.00" } } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v2/rate_cards/{rate_card_id}/rates"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rates"].as_array().unwrap().len(), 1);

    // plan applied rate card + rate phase
    let pl = plan(org.id, "starter");
    db.insert_plan(&pl).await.unwrap();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/plans/starter/applied_rate_cards",
        Some(json!({ "applied_rate_card": { "rate_card_id": rate_card_id } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let applied_id = body["applied_rate_card"]["lago_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v2/applied_rate_cards/{applied_id}/rate_phases"),
        Some(json!({ "rate_phase": { "code": "phase-1", "position": 1 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v2/applied_rate_cards/{applied_id}/rate_phases"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rate_phases"].as_array().unwrap().len(), 1);
}

// ---- add-ons ---------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn add_on_crud(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, _) = call(&app, "POST", "/api/v1/add_ons", Some(json!({ "add_on": { "name": "Setup", "code": "setup", "amount_cents": 5000, "amount_currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_add_on_by_code(org.id, "setup")
        .await
        .unwrap()
        .is_some());

    let (status, body) = call(&app, "GET", "/api/v1/add_ons", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["add_ons"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/add_ons/setup",
        Some(json!({ "add_on": { "amount_cents": 7500 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["add_on"]["amount_cents"], 7500);

    let (status, _) = call(&app, "DELETE", "/api/v1/add_ons/setup", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_add_on_by_code(org.id, "setup")
        .await
        .unwrap()
        .is_none());
}

// ---- wallets index/update/terminate ----------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn wallet_index_update_terminate(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();

    let (status, body) = call(&app, "POST", "/api/v1/wallets", Some(json!({ "wallet": { "external_customer_id": "cust-1", "currency": "USD", "rate_amount": "1" } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let wallet_id = body["wallet"]["lago_id"].as_str().unwrap().to_string();

    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/wallets?external_customer_id=cust-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["wallets"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/wallets/{wallet_id}"),
        Some(json!({ "wallet": { "name": "Prepaid" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["wallet"]["name"], "Prepaid");

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v1/wallets/{wallet_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["wallet"]["status"], "terminated");
}

// ---- features + analytics --------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn features_and_analytics(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, body) = call(&app, "POST", "/api/v1/features", Some(json!({ "feature": { "code": "seats", "name": "Seats", "privileges": [{ "code": "max", "value_type": "integer" }] } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(db
        .find_feature_by_code(org.id, "seats")
        .await
        .unwrap()
        .is_some());

    let (status, body) = call(&app, "GET", "/api/v1/features", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["features"].as_array().unwrap().len(), 1);

    let (status, body) = call(&app, "GET", "/api/v1/features/seats", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["feature"]["code"], "seats");

    // analytics endpoints resolve (empty aggregates on a fresh org)
    for path in [
        "gross_revenue",
        "mrr",
        "overdue_balance",
        "invoice_collection",
        "invoiced_usage",
    ] {
        let (status, _) = call(&app, "GET", &format!("/api/v1/analytics/{path}"), None).await;
        assert_eq!(status, StatusCode::OK, "analytics/{path}");
    }
}

// ---- credit note index + void ----------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn credit_note_index_and_void(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();
    // a fee is required for the estimate/create path; use a minimal one-off fee.
    let now = Utc::now();
    let fee = m::Fee {
        id: Uuid::new_v4(),
        organization_id: org.id,
        billing_entity_id: org.id,
        invoice_id: Some(inv.id),
        subscription_id: None,
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(FeeType::AddOn),
        invoiceable_type: Some("AddOn".into()),
        invoiceable_id: None,
        amount_cents: 1000,
        amount_currency: "USD".into(),
        precise_amount_cents: rust_decimal::Decimal::from(1000),
        unit_amount_cents: 1000,
        precise_unit_amount: rust_decimal::Decimal::from(10),
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: rust_decimal::Decimal::ZERO,
        taxes_rate: 0.0,
        units: rust_decimal::Decimal::from(1),
        total_aggregated_units: Some(rust_decimal::Decimal::from(1)),
        events_count: None,
        precise_coupons_amount_cents: rust_decimal::Decimal::ZERO,
        precise_credit_notes_amount_cents: rust_decimal::Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({}),
        grouped_by: json!({}),
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    };
    db.insert_fee(&fee).await.unwrap();

    let (status, body) = call(&app, "POST", "/api/v1/credit_notes", Some(json!({ "credit_note": { "invoice_id": inv.id, "items": [{ "fee_id": fee.id, "amount_cents": 500 }] } }))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let cn_id = body["credit_note"]["lago_id"].as_str().unwrap().to_string();

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/credit_notes?invoice_id={}", inv.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credit_notes"].as_array().unwrap().len(), 1);

    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/v1/credit_notes/{cn_id}/void"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---- applied coupons index (org + customer scoped) -------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn applied_coupons_index(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();

    let (status, _) = call(&app, "POST", "/api/v1/coupons", Some(json!({ "coupon": { "name": "Welcome", "code": "welcome", "coupon_type": "fixed_amount", "amount_cents": 500, "amount_currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "POST", "/api/v1/applied_coupons", Some(json!({ "applied_coupon": { "external_customer_id": "cust-1", "coupon_code": "welcome" } }))).await;
    assert_eq!(status, StatusCode::OK);

    // org-scoped index carries the resolved coupon code + customer external id
    let (status, body) = call(&app, "GET", "/api/v1/applied_coupons", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied_coupons"].as_array().unwrap().len(), 1);
    assert_eq!(body["applied_coupons"][0]["coupon_code"], "welcome");
    assert_eq!(body["applied_coupons"][0]["external_customer_id"], "cust-1");

    // nested customer-scoped index + org-scoped filter both scope to the customer
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/customers/cust-1/applied_coupons",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied_coupons"].as_array().unwrap().len(), 1);
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/applied_coupons?external_customer_id=cust-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied_coupons"].as_array().unwrap().len(), 1);
    assert_eq!(
        db.list_applied_coupons(org.id, Some(c.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

// ---- customer-scoped credit notes + payments -------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn customer_scoped_credit_notes_and_payments(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();
    let now = Utc::now();
    let fee = m::Fee {
        id: Uuid::new_v4(),
        organization_id: org.id,
        billing_entity_id: org.id,
        invoice_id: Some(inv.id),
        subscription_id: None,
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(FeeType::AddOn),
        invoiceable_type: Some("AddOn".into()),
        invoiceable_id: None,
        amount_cents: 1000,
        amount_currency: "USD".into(),
        precise_amount_cents: rust_decimal::Decimal::from(1000),
        unit_amount_cents: 1000,
        precise_unit_amount: rust_decimal::Decimal::from(10),
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: rust_decimal::Decimal::ZERO,
        taxes_rate: 0.0,
        units: rust_decimal::Decimal::from(1),
        total_aggregated_units: Some(rust_decimal::Decimal::from(1)),
        events_count: None,
        precise_coupons_amount_cents: rust_decimal::Decimal::ZERO,
        precise_credit_notes_amount_cents: rust_decimal::Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({}),
        grouped_by: json!({}),
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    };
    db.insert_fee(&fee).await.unwrap();

    // create a credit note + a payment against the customer's invoice
    let (status, _) = call(&app, "POST", "/api/v1/credit_notes", Some(json!({ "credit_note": { "invoice_id": inv.id, "items": [{ "fee_id": fee.id, "amount_cents": 500 }] } }))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "POST", "/api/v1/payments", Some(json!({ "payment": { "invoice_id": inv.id, "amount_cents": 1000, "amount_currency": "USD" } }))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&app, "GET", "/api/v1/customers/cust-1/credit_notes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credit_notes"].as_array().unwrap().len(), 1);
    assert_eq!(db.credit_notes_for_customer(c.id).await.unwrap().len(), 1);

    let (status, body) = call(&app, "GET", "/api/v1/customers/cust-1/payments", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["payments"].as_array().unwrap().len(), 1);
    assert_eq!(db.payments_for_customer(c.id).await.unwrap().len(), 1);
}

// ---- feature update + destroy ----------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn feature_update_and_destroy(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/features",
        Some(json!({ "feature": { "code": "seats", "name": "Seats" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v1/features/seats",
        Some(json!({ "feature": { "name": "Seats+", "description": "seat entitlement" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["feature"]["name"], "Seats+");
    let (_, _, name, desc) = db.find_feature(org.id, "seats").await.unwrap().unwrap();
    assert_eq!(name.as_deref(), Some("Seats+"));
    assert_eq!(desc.as_deref(), Some("seat entitlement"));

    let (status, _) = call(&app, "DELETE", "/api/v1/features/seats", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(db
        .find_feature_by_code(org.id, "seats")
        .await
        .unwrap()
        .is_none());
}

// ---- projected + past usage (reuse the usage engine) -----------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn projected_and_past_usage(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();
    let pl = plan(org.id, "starter");
    db.insert_plan(&pl).await.unwrap();
    let sub = m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: c.id,
        plan_id: pl.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-1".into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(Utc::now()),
        started_at: Some(Utc::now()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    db.insert_subscription(&sub).await.unwrap();

    // projected_usage reuses the current-usage engine + adds the projection block
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/customers/cust-1/projected_usage?external_subscription_id=sub-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["customer_projected_usage"].is_object());
    assert!(body["customer_projected_usage"]["projected_amount_cents"].is_number());
    assert_eq!(body["customer_projected_usage"]["currency"], "USD");

    // past_usage: no finalized periods yet → empty collection, 200
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/customers/cust-1/past_usage?external_subscription_id=sub-1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["usage_periods"].as_array().unwrap().len(), 0);
    assert_eq!(
        db.subscription_past_usage_periods(sub.id)
            .await
            .unwrap()
            .len(),
        0
    );
}

// ---- webhook emit at REST trigger points -----------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn rest_handlers_emit_webhooks_endpoint_gated(pool: PgPool) {
    let (app, db, org) = setup(pool).await;

    // No endpoint configured → customer.created emit is a no-op.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/customers",
        Some(json!({ "customer": { "external_id": "cust-noop" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let recent = db.recent_webhooks(org.id, 50).await.unwrap();
    assert!(
        !recent.iter().any(|(t, _, _)| t == "customer.created"),
        "no endpoint → no webhook row"
    );

    // Configure an endpoint; create fires customer.created, re-post fires
    // customer.updated (upsert), and feature create fires feature.created.
    db.insert_webhook_endpoint_returning(org.id, "https://hook.test/a")
        .await
        .unwrap();

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/customers",
        Some(json!({ "customer": { "external_id": "cust-1", "name": "Acme" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/customers",
        Some(json!({ "customer": { "external_id": "cust-1", "name": "Acme Renamed" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/features",
        Some(json!({ "feature": { "code": "seats", "name": "Seats" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let recent = db.recent_webhooks(org.id, 50).await.unwrap();
    assert!(
        recent
            .iter()
            .any(|(t, s, _)| t == "customer.created" && s == "pending"),
        "customer.created row: {recent:?}"
    );
    assert!(
        recent
            .iter()
            .any(|(t, s, _)| t == "customer.updated" && s == "pending"),
        "customer.updated row: {recent:?}"
    );
    assert!(
        recent
            .iter()
            .any(|(t, s, _)| t == "feature.created" && s == "pending"),
        "feature.created row: {recent:?}"
    );
}

// ---- dunning campaigns + thresholds ----------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn dunning_campaign_with_thresholds_round_trips(pool: PgPool) {
    let (app, _db, _org) = setup(pool).await;

    // Create a campaign carrying per-currency thresholds.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/dunning_campaigns",
        Some(json!({ "dunning_campaign": {
            "name": "Default", "code": "default",
            "max_attempts": 3, "days_between_attempts": 7,
            "applied_to_organization": true,
            "thresholds": [
                { "currency": "USD", "amount_cents": 5000 },
                { "currency": "EUR", "amount_cents": 10000 }
            ]
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["dunning_campaign"]["thresholds"].as_array().unwrap().len(),
        2
    );

    // List echoes the persisted thresholds.
    let (status, body) = call(&app, "GET", "/api/v1/dunning_campaigns", None).await;
    assert_eq!(status, StatusCode::OK);
    let campaigns = body["dunning_campaigns"].as_array().unwrap();
    assert_eq!(campaigns.len(), 1);
    let thresholds = campaigns[0]["thresholds"].as_array().unwrap();
    assert_eq!(thresholds.len(), 2);
    // Ordered by amount_cents ascending (see `dunning_campaign_thresholds`).
    assert_eq!(thresholds[0]["currency"], "USD");
    assert_eq!(thresholds[0]["amount_cents"], 5000);
    assert_eq!(thresholds[1]["currency"], "EUR");
    assert_eq!(thresholds[1]["amount_cents"], 10000);
}

// ---- auth guard ------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn unauthenticated_is_rejected(pool: PgPool) {
    let (app, _db, _org) = setup(pool).await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/taxes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

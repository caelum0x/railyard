// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! `/plans/:code/fixed_charges` (Lago Plans::FixedChargesController) and the
//! plan charge reads (`GET /plans/:code/charges[/:id]`); a fixed charge made
//! here is billed on the invoice preview.

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

const TOKEN: &str = "fixed-charges-key";

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn setup(pool: PgPool) -> (axum::Router, Db, m::Organization) {
    let db = Db::new(pool);
    let now = Utc::now();
    let org = m::Organization {
        id: Uuid::new_v4(),
        name: "Acme".into(),
        slug: format!("acme-{}", &Uuid::new_v4().to_string()[..8]),
        default_currency: "USD".into(),
        email: None,
        country: None,
        tax_identification_number: None,
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: true,
        hmac_key: Some(Uuid::new_v4().to_string()),
        created_at: now,
        updated_at: now,
    };
    db.insert_organization(&org).await.unwrap();
    db.insert_api_key(&m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org.id,
        value: TOKEN.into(),
        name: None,
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();
    (rago_api::router(AppState::new(db.clone())), db, org)
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn plan_fixed_charges_crud_and_billing(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let ok = |r: (StatusCode, Value)| {
        assert_eq!(r.0, StatusCode::OK, "{}", r.1);
        r.1
    };
    ok(call(
        &app,
        "POST",
        "/api/v1/add_ons",
        Some(json!({ "add_on": { "name": "Seat", "code": "seat", "amount_cents": 1200, "amount_currency": "USD" } })),
    )
    .await);
    ok(call(&app, "POST", "/api/v1/plans", Some(json!({ "plan": { "name": "Team", "code": "team", "interval": "monthly", "amount_cents": 0, "amount_currency": "USD" } }))).await);

    let created = ok(call(&app, "POST", "/api/v1/plans/team/fixed_charges", Some(json!({ "fixed_charge": {
        "add_on_code": "seat", "charge_model": "standard", "units": "5", "properties": { "amount": "12" }, "invoice_display_name": "Seats"
    }}))).await);
    assert_eq!(created["fixed_charge"]["code"], "seat");
    assert_eq!(created["fixed_charge"]["add_on_code"], "seat");
    assert_eq!(created["fixed_charge"]["units"], "5");

    // Same code twice, an unknown add-on, a usage-only model and negative units are refused.
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/plans/team/fixed_charges",
            Some(json!({ "fixed_charge": { "add_on_code": "seat", "units": 1 } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/plans/team/fixed_charges",
            Some(json!({ "fixed_charge": { "add_on_code": "nope", "units": 1 } }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/plans/team/fixed_charges",
            Some(json!({ "fixed_charge": { "add_on_code": "seat", "code": "x", "charge_model": "percentage" } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/plans/team/fixed_charges",
            Some(json!({ "fixed_charge": { "add_on_code": "seat", "code": "y", "units": -1 } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let list = ok(call(&app, "GET", "/api/v1/plans/team/fixed_charges", None).await);
    assert_eq!(list["fixed_charges"].as_array().unwrap().len(), 1);
    let updated = ok(call(
        &app,
        "PUT",
        "/api/v1/plans/team/fixed_charges/seat",
        Some(json!({ "fixed_charge": { "units": 8 } })),
    )
    .await);
    assert_eq!(updated["fixed_charge"]["units"], "8");
    let plan = ok(call(&app, "GET", "/api/v1/plans/team", None).await);
    assert_eq!(plan["plan"]["fixed_charges"][0]["code"], "seat");

    // Billed: 8 seats at $12 on the preview of a subscription to the plan.
    let now = Utc::now();
    let customer = m::Customer {
        id: Uuid::new_v4(),
        organization_id: org.id,
        billing_entity_id: org.id,
        external_id: "cust-fc".into(),
        name: None,
        email: None,
        country: None,
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    };
    db.insert_customer(&customer).await.unwrap();
    let team = db.find_plan_by_code(org.id, "team").await.unwrap().unwrap();
    let started = now - chrono::Duration::days(400);
    db.insert_subscription(&m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: customer.id,
        plan_id: team.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-fc".into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(started),
        started_at: Some(started),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();
    let preview = ok(call(
        &app,
        "POST",
        "/api/v1/invoices/preview",
        Some(json!({ "subscriptions": { "external_ids": ["sub-fc"] } })),
    )
    .await);
    assert_eq!(preview["invoice"]["total_amount_cents"], 9_600, "{preview}");

    let gone = ok(call(&app, "DELETE", "/api/v1/plans/team/fixed_charges/seat", None).await);
    assert_eq!(gone["fixed_charge"]["code"], "seat");
    assert_eq!(
        call(&app, "GET", "/api/v1/plans/team/fixed_charges/seat", None).await.0,
        StatusCode::NOT_FOUND
    );
    let preview = ok(call(
        &app,
        "POST",
        "/api/v1/invoices/preview",
        Some(json!({ "subscriptions": { "external_ids": ["sub-fc"] } })),
    )
    .await);
    assert_eq!(preview["invoice"]["total_amount_cents"], 0);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn plan_charges_are_readable(pool: PgPool) {
    let (app, _db, _org) = setup(pool).await;
    let ok = |r: (StatusCode, Value)| {
        assert_eq!(r.0, StatusCode::OK, "{}", r.1);
        r.1
    };
    ok(call(
        &app,
        "POST",
        "/api/v1/billable_metrics",
        Some(json!({ "billable_metric": { "name": "Calls", "code": "calls", "aggregation_type": "count_agg" } })),
    )
    .await);
    ok(call(&app, "POST", "/api/v1/plans", Some(json!({ "plan": { "name": "Pro", "code": "pro", "interval": "monthly", "amount_cents": 0, "amount_currency": "USD",
        "charges": [{ "billable_metric_code": "calls", "charge_model": "standard", "properties": { "amount": "1" } }] } }))).await);
    let list = ok(call(&app, "GET", "/api/v1/plans/pro/charges", None).await);
    let charges = list["charges"].as_array().unwrap();
    assert_eq!(charges.len(), 1, "{list}");
    let id = charges[0]["lago_id"].as_str().unwrap().to_string();
    let one = ok(call(&app, "GET", &format!("/api/v1/plans/pro/charges/{id}"), None).await);
    assert_eq!(one["charge"]["lago_id"], id);
    let by_code = ok(call(&app, "GET", "/api/v1/plans/pro/charges/calls", None).await);
    assert_eq!(by_code["charge"]["lago_id"], id);
    assert_eq!(
        call(&app, "GET", "/api/v1/plans/pro/charges/nope", None).await.0,
        StatusCode::NOT_FOUND
    );
}

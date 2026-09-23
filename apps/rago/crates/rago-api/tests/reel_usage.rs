// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Live-DB integration tests for the Menivor reel-billing surface (task T22):
//! `POST /reels/usage` (usage ingest) and
//! `POST /customers/:external_customer_id/render_allowance` (the pre-render
//! credit/cap gate).
//!
//! Each test runs on an isolated, migrated Postgres via
//! `#[sqlx::test(migrator = "rago_db::MIGRATOR")]` (DB-less CI skips these —
//! sqlx requires `DATABASE_URL`). Every case drives a real axum request through
//! `rago_api::router` with a Bearer api-key, then asserts both the returned
//! Lago-shaped JSON and the persisted DB change.
//!
//! Coverage mirrors the three behaviours the reel path promises:
//!   * one render expands into the billable usage events it should emit
//!     (`reel_usage_events`), each persisted through the shared event path;
//!   * re-reporting the same `render_id` is idempotent (same render billed
//!     exactly once, the replay surfaced as `already_billed_metrics`);
//!   * `render_allowance` denies on `insufficient_credits` /
//!     `usage_cap_exceeded` and allows otherwise (incl. the postpaid case where
//!     a customer with no active prepaid wallet is never blocked by credits).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use rust_decimal::Decimal;
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
        name: "Menivor".into(),
        slug: format!("menivor-{}", &Uuid::new_v4().to_string()[..8]),
        default_currency: "USD".into(),
        email: Some("ops@menivor.test".into()),
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
        name: Some("Creator".into()),
        email: Some("c@menivor.test".into()),
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
        name: "Menivor Reels".into(),
        code: code.into(),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(0),
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

/// A billable metric with a caller-chosen aggregation + optional `field_name`
/// (required for the `sum_agg` reel dimensions).
fn metric(
    org_id: Uuid,
    code: &str,
    aggregation_type: AggregationType,
    field_name: Option<&str>,
) -> m::BillableMetric {
    let now = Utc::now();
    m::BillableMetric {
        id: Uuid::new_v4(),
        organization_id: org_id,
        name: code.into(),
        code: code.into(),
        description: None,
        aggregation_type,
        field_name: field_name.map(|s| s.into()),
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

fn subscription(org_id: Uuid, customer_id: Uuid, plan_id: Uuid, external_id: &str) -> m::Subscription {
    let now = Utc::now();
    m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org_id,
        customer_id,
        plan_id,
        billing_entity_id: Some(org_id),
        external_id: external_id.into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(now),
        started_at: Some(now),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    }
}

/// A prepaid wallet for `customer_id`. `ongoing_balance_cents` is the figure the
/// allowance gate compares the estimated render cost against; `status` decides
/// whether the customer is prepaid (Active) or postpaid (Terminated → ignored).
fn wallet(org_id: Uuid, customer_id: Uuid, ongoing_balance_cents: i64, status: WalletStatus) -> m::Wallet {
    let now = Utc::now();
    m::Wallet {
        id: Uuid::new_v4(),
        organization_id: org_id,
        customer_id,
        status,
        name: Some("Credits".into()),
        rate_amount: Decimal::ONE,
        credits_balance: Decimal::ZERO,
        consumed_credits: Decimal::ZERO,
        balance_cents: ongoing_balance_cents,
        balance_currency: "USD".into(),
        consumed_amount_cents: 0,
        ongoing_balance_cents,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 1,
        created_at: now,
        updated_at: now,
    }
}

/// Provision the full reel-billing scope an operator would set up: the customer,
/// the reel plan, the three reel billable metrics (see
/// `rago_api::emit::reel_metric`), and an active subscription. Returns the
/// customer and subscription external ids for driving the endpoints.
async fn seed_reel_scope(db: &Db, org: &m::Organization) -> (String, String) {
    let cust = customer(org.id, "cust-reel");
    db.insert_customer(&cust).await.unwrap();
    let pl = plan(org.id, "menivor_reels");
    db.insert_plan(&pl).await.unwrap();
    db.insert_billable_metric(&metric(org.id, "reel_generated", AggregationType::CountAgg, None))
        .await
        .unwrap();
    db.insert_billable_metric(&metric(
        org.id,
        "reel_render_seconds",
        AggregationType::SumAgg,
        Some("seconds"),
    ))
    .await
    .unwrap();
    db.insert_billable_metric(&metric(
        org.id,
        "reel_ai_credits",
        AggregationType::SumAgg,
        Some("credits"),
    ))
    .await
    .unwrap();
    let sub = subscription(org.id, cust.id, pl.id, "sub-reel");
    db.insert_subscription(&sub).await.unwrap();
    (cust.external_id, sub.external_id)
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

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
    let resp = app.clone().oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Collect the `code` of every billed event in a `POST /reels/usage` response.
fn billed_codes(body: &Value) -> Vec<String> {
    body["reel_usage"]["billed_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

// ---- reel usage: expansion -------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_expands_into_one_event_per_dimension(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    // A render that generated a reel and consumed both metered quantities emits
    // all three dimensions.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({
            "render_id": "render-1",
            "external_subscription_id": sub_ext,
            "generated": true,
            "render_seconds": 30,
            "ai_credits": 5,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let mut codes = billed_codes(&body);
    codes.sort();
    assert_eq!(
        codes,
        vec!["reel_ai_credits", "reel_generated", "reel_render_seconds"],
    );
    assert!(body["reel_usage"]["already_billed_metrics"]
        .as_array()
        .unwrap()
        .is_empty());

    // Each dimension persisted exactly one event under `{render_id}:{code}`, and
    // the sum_agg dimensions carry their numeric property.
    for code in ["reel_generated", "reel_render_seconds", "reel_ai_credits"] {
        assert_eq!(
            db.count_events(org.id, Some(code), Some(&sub_ext)).await.unwrap(),
            1,
            "expected one {code} event",
        );
        assert!(
            db.event_exists(org.id, &sub_ext, &format!("render-1:{code}")).await.unwrap(),
            "expected transaction render-1:{code}",
        );
    }
    let events = db.list_events(org.id, Some("reel_render_seconds"), Some(&sub_ext), 10, 0).await.unwrap();
    assert_eq!(events[0].properties["seconds"], 30);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_omits_absent_and_nonpositive_dimensions(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    // No reel produced, zero render seconds (non-positive → skipped), only AI
    // credits present: exactly one event, for `reel_ai_credits`.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({
            "render_id": "render-partial",
            "external_subscription_id": sub_ext,
            "generated": false,
            "render_seconds": 0,
            "ai_credits": 8,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    assert_eq!(billed_codes(&body), vec!["reel_ai_credits"]);
    assert_eq!(db.count_events(org.id, Some("reel_generated"), Some(&sub_ext)).await.unwrap(), 0);
    assert_eq!(db.count_events(org.id, Some("reel_render_seconds"), Some(&sub_ext)).await.unwrap(), 0);
    assert_eq!(db.count_events(org.id, Some("reel_ai_credits"), Some(&sub_ext)).await.unwrap(), 1);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_defaults_generated_to_true(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    // `generated` omitted defaults to true; no quantities → just the count event.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({ "render_id": "render-default", "external_subscription_id": sub_ext })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(billed_codes(&body), vec!["reel_generated"]);
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 1);
}

// ---- reel usage: idempotency ----------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_same_render_id_charges_once(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    let payload = json!({
        "render_id": "render-dup",
        "external_subscription_id": sub_ext,
        "generated": true,
        "render_seconds": 12,
        "ai_credits": 3,
    });

    // First report bills all three dimensions.
    let (status, first) = call(&app, "POST", "/api/v1/reels/usage", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK, "{first:?}");
    assert_eq!(billed_codes(&first).len(), 3);
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 3);

    // Replaying the identical render bills nothing new: the derived
    // `{render_id}:{code}` transaction ids are recognised as already billed and
    // surfaced under `already_billed_metrics`, not re-inserted.
    let (status, second) = call(&app, "POST", "/api/v1/reels/usage", Some(payload)).await;
    assert_eq!(status, StatusCode::OK, "{second:?}");
    assert!(second["reel_usage"]["billed_events"].as_array().unwrap().is_empty());
    let mut replayed = second["reel_usage"]["already_billed_metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    replayed.sort();
    assert_eq!(
        replayed,
        vec!["reel_ai_credits", "reel_generated", "reel_render_seconds"],
    );

    // Still exactly three events — the render was charged once.
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 3);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_partial_replay_bills_only_new_dimensions(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    // First render only produced a reel (no quantities).
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({ "render_id": "render-grow", "external_subscription_id": sub_ext, "generated": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 1);

    // A corrected report for the SAME render_id now also carries render seconds:
    // the already-billed `reel_generated` is skipped, the new dimension bills.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({
            "render_id": "render-grow",
            "external_subscription_id": sub_ext,
            "generated": true,
            "render_seconds": 20,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(billed_codes(&body), vec!["reel_render_seconds"]);
    assert_eq!(
        body["reel_usage"]["already_billed_metrics"],
        json!(["reel_generated"]),
    );
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 2);
}

// ---- reel usage: validation ------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_rejects_blank_render_id(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (_cust, sub_ext) = seed_reel_scope(&db, &org).await;

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({ "render_id": "   ", "external_subscription_id": sub_ext })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(db.count_events(org.id, None, Some(&sub_ext)).await.unwrap(), 0);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_rejects_blank_subscription(pool: PgPool) {
    let (app, _db, _org) = setup(pool).await;

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({ "render_id": "render-x", "external_subscription_id": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn reel_usage_unknown_subscription_is_not_found(pool: PgPool) {
    let (app, _db, _org) = setup(pool).await;
    // No subscription seeded: ingest surfaces the shared event-path 404 for an
    // unknown subscription (not a silent bill).
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/reels/usage",
        Some(json!({ "render_id": "render-x", "external_subscription_id": "ghost-sub" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- render allowance: credit + cap branches -------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_denies_insufficient_credits(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;
    let cust = db.find_customer_by_external_id(org.id, &cust_ext).await.unwrap().unwrap();
    // Active prepaid wallet holds 100¢; the render is estimated at 500¢.
    db.insert_wallet(&wallet(org.id, cust.id, 100, WalletStatus::Active)).await.unwrap();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": 500 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["render_allowance"]["allowed"], false);
    assert_eq!(body["render_allowance"]["reasons"], json!(["insufficient_credits"]));
    assert_eq!(body["render_allowance"]["has_prepaid_wallet"], true);
    assert_eq!(body["render_allowance"]["credit_balance_cents"], 100);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_allows_within_credits(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;
    let cust = db.find_customer_by_external_id(org.id, &cust_ext).await.unwrap().unwrap();
    db.insert_wallet(&wallet(org.id, cust.id, 1000, WalletStatus::Active)).await.unwrap();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": 500 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["render_allowance"]["allowed"], true);
    assert!(body["render_allowance"]["reasons"].as_array().unwrap().is_empty());
    assert_eq!(body["render_allowance"]["projected_usage_cents"], 500);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_denies_usage_cap_exceeded(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;
    let cust = db.find_customer_by_external_id(org.id, &cust_ext).await.unwrap().unwrap();
    // Credits are ample; the spend cap is what bites (projected 500 > cap 100).
    db.insert_wallet(&wallet(org.id, cust.id, 100_000, WalletStatus::Active)).await.unwrap();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": 500, "cap_cents": 100 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["render_allowance"]["allowed"], false);
    assert_eq!(body["render_allowance"]["reasons"], json!(["usage_cap_exceeded"]));
    assert_eq!(body["render_allowance"]["cap_cents"], 100);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_reports_both_reasons(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;
    let cust = db.find_customer_by_external_id(org.id, &cust_ext).await.unwrap().unwrap();
    // Thin credits AND a low cap: both gates deny.
    db.insert_wallet(&wallet(org.id, cust.id, 50, WalletStatus::Active)).await.unwrap();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": 500, "cap_cents": 100 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["render_allowance"]["allowed"], false);
    assert_eq!(
        body["render_allowance"]["reasons"],
        json!(["insufficient_credits", "usage_cap_exceeded"]),
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_postpaid_customer_ignores_credits(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;
    let cust = db.find_customer_by_external_id(org.id, &cust_ext).await.unwrap().unwrap();
    // Only a TERMINATED wallet exists → the customer is postpaid; credits never
    // block, and with no cap the render is allowed regardless of cost.
    db.insert_wallet(&wallet(org.id, cust.id, 0, WalletStatus::Terminated)).await.unwrap();

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": 99_999 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["render_allowance"]["allowed"], true);
    assert_eq!(body["render_allowance"]["has_prepaid_wallet"], false);
    assert_eq!(body["render_allowance"]["credit_balance_cents"], 0);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn render_allowance_rejects_negative_estimate(pool: PgPool) {
    let (app, db, org) = setup(pool).await;
    let (cust_ext, sub_ext) = seed_reel_scope(&db, &org).await;

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/customers/{cust_ext}/render_allowance"),
        Some(json!({ "external_subscription_id": sub_ext, "estimated_cost_cents": -1 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

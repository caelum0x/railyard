// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! REST catalog and lifecycle changes emit their webhooks like the GraphQL
//! paths: billable_metric.created/updated/deleted, plan.created/updated/
//! deleted, subscription.updated and wallet.terminated, each with the same
//! object the REST answer carries.

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

const TOKEN: &str = "webhook-paths-key";

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

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn rest_changes_emit_their_webhooks(pool: PgPool) {
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
    db.insert_webhook_endpoint(org.id, "https://hooks.example.com/rago")
        .await
        .unwrap();
    let app = rago_api::router(AppState::new(db.clone()));

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
    ok(call(
        &app,
        "PUT",
        "/api/v1/billable_metrics/calls",
        Some(json!({ "billable_metric": { "name": "API calls" } })),
    )
    .await);
    ok(call(&app, "POST", "/api/v1/plans", Some(json!({ "plan": { "name": "Pro", "code": "pro", "interval": "monthly", "amount_cents": 1000, "amount_currency": "USD" } }))).await);
    ok(call(
        &app,
        "PUT",
        "/api/v1/plans/pro",
        Some(json!({ "plan": { "name": "Pro+" } })),
    )
    .await);

    let customer = m::Customer {
        id: Uuid::new_v4(),
        organization_id: org.id,
        billing_entity_id: org.id,
        external_id: "cust-w".into(),
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
    let plan = db.find_plan_by_code(org.id, "pro").await.unwrap().unwrap();
    db.insert_subscription(&m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: customer.id,
        plan_id: plan.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-w".into(),
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
    })
    .await
    .unwrap();
    ok(call(
        &app,
        "PUT",
        "/api/v1/subscriptions/sub-w",
        Some(json!({ "subscription": { "name": "Main" } })),
    )
    .await);
    let w = ok(call(
        &app,
        "POST",
        "/api/v1/wallets",
        Some(json!({ "wallet": { "external_customer_id": "cust-w", "rate_amount": "1", "currency": "USD" } })),
    )
    .await);
    let wallet_id = w["wallet"]["lago_id"].as_str().unwrap().to_string();
    ok(call(&app, "DELETE", &format!("/api/v1/wallets/{wallet_id}"), None).await);
    // A second terminate changes nothing and emits nothing.
    call(&app, "DELETE", &format!("/api/v1/wallets/{wallet_id}"), None).await;
    ok(call(&app, "DELETE", "/api/v1/plans/pro", None).await);
    ok(call(&app, "DELETE", "/api/v1/billable_metrics/calls", None).await);

    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT webhook_type FROM webhooks WHERE organization_id = $1 ORDER BY created_at, id")
            .bind(org.id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    for expected in [
        "billable_metric.created",
        "billable_metric.updated",
        "plan.created",
        "plan.updated",
        "subscription.updated",
        "wallet.created",
        "wallet.terminated",
        "plan.deleted",
        "billable_metric.deleted",
    ] {
        assert_eq!(
            kinds.iter().filter(|k| *k == expected).count(),
            1,
            "{expected} in {kinds:?}"
        );
    }
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM webhooks WHERE organization_id = $1 AND webhook_type = 'plan.updated'")
            .bind(org.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(payload["plan"]["name"], "Pro+", "{payload}");
}

// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! REST ingest applies the billable metric's expression, and current usage is
//! priced per charge filter like the invoice.
//!
//! Before: only the Kafka path computed expressions, so an expression metric
//! fed through `POST /events` billed 0; and `current_usage` priced filtered
//! charges over every event at the default price.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use rago_api::AppState;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::charge_filters::ChargeFilterSpec;
use rago_db::Db;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "usage-lines-api-key";

struct App {
    app: axum::Router,
    db: Db,
    org: Uuid,
    sub: m::Subscription,
    customer: m::Customer,
}

async fn setup(pool: PgPool) -> App {
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
        name: Some("test".into()),
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();
    let customer = m::Customer {
        id: Uuid::new_v4(),
        organization_id: org.id,
        billing_entity_id: org.id,
        external_id: "cust-1".into(),
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
    let plan = m::Plan {
        id: Uuid::new_v4(),
        organization_id: org.id,
        name: "Usage".into(),
        code: "usage".into(),
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
    };
    db.insert_plan(&plan).await.unwrap();
    let started = now - chrono::Duration::days(400);
    let sub = m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: customer.id,
        plan_id: plan.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-1".into(),
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
    };
    db.insert_subscription(&sub).await.unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    App {
        app,
        db,
        org: org.id,
        sub,
        customer,
    }
}

impl App {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
        let resp = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn charge(&self, metric: Uuid, code: &str, properties: Value) -> Uuid {
        let now = Utc::now();
        let id = Uuid::new_v4();
        self.db
            .insert_charge(&m::Charge {
                id,
                organization_id: self.org,
                billable_metric_id: Some(metric),
                plan_id: Some(self.sub.plan_id),
                code: code.into(),
                charge_model: ChargeModel::Standard,
                properties,
                pay_in_advance: false,
                prorated: false,
                invoiceable: true,
                min_amount_cents: 0,
                amount_currency: Some("USD".into()),
                accepts_target_wallet: false,
                invoice_display_name: None,
                parent_id: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .unwrap();
        id
    }

    async fn current_usage(&self) -> Value {
        let (status, body) = self
            .call(
                "GET",
                &format!(
                    "/api/v1/customers/{}/current_usage?external_subscription_id={}",
                    self.customer.external_id, self.sub.external_id
                ),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["customer_usage"].clone()
    }
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn an_expression_metric_bills_the_computed_value_on_rest_ingest(pool: PgPool) {
    let a = setup(pool).await;
    // A broken expression and an expression without a field are refused.
    let (status, _) = a
        .call(
            "POST",
            "/api/v1/billable_metrics",
            Some(json!({ "billable_metric": {
                "name": "GPU hours", "code": "gpu_bad", "aggregation_type": "sum_agg", "field_name": "hours",
                "expression": "event.properties.ended_at -"
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = a
        .call(
            "POST",
            "/api/v1/billable_metrics",
            Some(json!({ "billable_metric": {
                "name": "GPU hours", "code": "gpu_nofield", "aggregation_type": "count_agg",
                "expression": "event.properties.units * 2"
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = a
        .call(
            "POST",
            "/api/v1/billable_metrics",
            Some(json!({ "billable_metric": {
                "name": "GPU hours", "code": "gpu", "aggregation_type": "sum_agg", "field_name": "hours",
                "expression": "round((event.properties.ended_at - event.properties.started_at) / 3600, 2)"
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let metric_id: Uuid = body["billable_metric"]["lago_id"].as_str().unwrap().parse().unwrap();
    a.charge(metric_id, "gpu", json!({ "amount": "2" })).await;

    let ts = Utc::now().timestamp() - 60;
    let (status, body) = a
        .call(
            "POST",
            "/api/v1/events",
            Some(json!({ "event": {
                "transaction_id": "gpu-1", "code": "gpu", "external_subscription_id": "sub-1", "timestamp": ts,
                "properties": { "started_at": 0, "ended_at": 5400 }
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["event"]["properties"]["hours"], json!("1.5"));

    // The batch path computes it too.
    let (status, body) = a
        .call(
            "POST",
            "/api/v2/events/batch",
            Some(json!({ "events": [{
                "transaction_id": "gpu-2", "code": "gpu", "external_subscription_id": "sub-1", "timestamp": ts,
                "properties": { "started_at": 0, "ended_at": 3600 }
            }]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created_count"], 1, "{body}");

    // An event the expression cannot evaluate is refused, not stored as 0.
    let (status, _) = a
        .call(
            "POST",
            "/api/v1/events",
            Some(json!({ "event": {
                "transaction_id": "gpu-3", "code": "gpu", "external_subscription_id": "sub-1", "timestamp": ts,
                "properties": { "started_at": 0 }
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 1.5 + 1 hours at $2.
    let usage = a.current_usage().await;
    assert_eq!(usage["amount_cents"], 500, "{usage}");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn current_usage_prices_each_filter_like_the_invoice(pool: PgPool) {
    let a = setup(pool).await;
    let (status, body) = a
        .call(
            "POST",
            "/api/v1/billable_metrics",
            Some(json!({ "billable_metric": {
                "name": "Storage", "code": "gb", "aggregation_type": "sum_agg", "field_name": "gb"
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let metric_id: Uuid = body["billable_metric"]["lago_id"].as_str().unwrap().parse().unwrap();
    let charge = a.charge(metric_id, "gb", json!({ "amount": "1" })).await;
    a.db.replace_charge_filters(
        a.org,
        charge,
        &[ChargeFilterSpec {
            invoice_display_name: None,
            properties: json!({ "amount": "5" }),
            values: json!({ "region": ["eu"] }),
        }],
    )
    .await
    .unwrap();
    let ts = Utc::now().timestamp() - 60;
    for (id, gb, region) in [("g1", 2, "eu"), ("g2", 3, "us")] {
        let (status, body) = a
            .call(
                "POST",
                "/api/v1/events",
                Some(json!({ "event": {
                    "transaction_id": id, "code": "gb", "external_subscription_id": "sub-1", "timestamp": ts,
                    "properties": { "gb": gb, "region": region }
                }})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let usage = a.current_usage().await;
    // eu 2 GB at $5 + us 3 GB at the default $1; before: 5 GB at $1 = 500.
    assert_eq!(usage["amount_cents"], 1_300, "{usage}");
    let line = &usage["charges_usage"][0];
    assert_eq!(line["units"], "5");
    let filters = line["filters"].as_array().unwrap();
    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0]["values"], json!({ "region": ["eu"] }));
    assert_eq!(filters[0]["amount_cents"], 1_000);
}

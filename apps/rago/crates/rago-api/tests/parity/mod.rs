//! Shared harness of the REST parity suites: an organization with an API key,
//! a router over a migrated database, and a JSON request helper.
#![allow(dead_code)]

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

pub struct Api {
    pub app: axum::Router,
    pub db: Db,
    pub org: m::Organization,
    pub token: String,
}

pub async fn api(pool: PgPool) -> Api {
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
    let token = format!("key-{}", Uuid::new_v4());
    db.insert_api_key(&m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org.id,
        value: token.clone(),
        name: None,
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();
    let app = rago_api::router(AppState::new(db.clone()));
    Api { app, db, org, token }
}

impl Api {
    pub async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call_as(&self.token, method, uri, body).await
    }

    pub async fn call_as(&self, token: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
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

    /// 200 or panic with the body.
    pub async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let (status, v) = self.call(method, uri, body).await;
        assert_eq!(status, StatusCode::OK, "{method} {uri}: {v}");
        v
    }

    pub async fn customer(&self, external_id: &str) -> m::Customer {
        let now = Utc::now();
        let c = m::Customer {
            id: Uuid::new_v4(),
            organization_id: self.org.id,
            billing_entity_id: self.org.id,
            external_id: external_id.into(),
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
        self.db.insert_customer(&c).await.unwrap();
        c
    }

    /// A second organization's token (for isolation checks).
    pub async fn other_org_token(&self) -> String {
        let now = Utc::now();
        let org = m::Organization {
            id: Uuid::new_v4(),
            name: "Rival".into(),
            slug: format!("rival-{}", &Uuid::new_v4().to_string()[..8]),
            default_currency: "USD".into(),
            email: None,
            country: None,
            tax_identification_number: None,
            timezone: "UTC".into(),
            document_locale: "en".into(),
            finalize_zero_amount_invoice: true,
            hmac_key: None,
            created_at: now,
            updated_at: now,
        };
        self.db.insert_organization(&org).await.unwrap();
        let token = format!("key-{}", Uuid::new_v4());
        self.db
            .insert_api_key(&m::ApiKey {
                id: Uuid::new_v4(),
                organization_id: org.id,
                value: token.clone(),
                name: None,
                permissions: json!({}),
                expires_at: None,
                last_used_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
        token
    }

    /// A monthly plan with one standard charge on a count metric, and an
    /// active subscription of `customer` to it (started a year ago).
    pub async fn plan_and_subscription(
        &self,
        customer: &m::Customer,
        plan_code: &str,
        sub_ext: &str,
    ) -> (m::Plan, m::Subscription) {
        self.ok("POST", "/api/v1/billable_metrics", Some(json!({ "billable_metric": { "name": "Calls", "code": format!("calls_{plan_code}"), "aggregation_type": "count_agg" } }))).await;
        self.ok(
            "POST",
            "/api/v1/plans",
            Some(json!({ "plan": { "name": "Pro", "code": plan_code, "interval": "monthly", "amount_cents": 1000, "amount_currency": "USD",
                "charges": [{ "billable_metric_code": format!("calls_{plan_code}"), "charge_model": "standard", "properties": { "amount": "1" } }] } })),
        )
        .await;
        let plan = self
            .db
            .find_plan_by_code(self.org.id, plan_code)
            .await
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let started = now - chrono::Duration::days(365);
        let sub = m::Subscription {
            id: Uuid::new_v4(),
            organization_id: self.org.id,
            customer_id: customer.id,
            plan_id: plan.id,
            billing_entity_id: Some(self.org.id),
            external_id: sub_ext.into(),
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
        self.db.insert_subscription(&sub).await.unwrap();
        (plan, sub)
    }
}

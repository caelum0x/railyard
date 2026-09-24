// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Terminating a subscription that has a scheduled downgrade: the downgrade is
//! canceled only AFTER the prorated final invoice is issued. If the final
//! invoice fails, the subscription stays active with its downgrade intact so a
//! retry picks both up again (`handlers::terminate_subscription_now`).
//!
//! The failure is injected at the database: a trigger refuses invoice inserts
//! for the first termination attempt, then is dropped for the retry.

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

const KEY: &str = "downgrade-term-key";

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
        external_id: "cust-dg".into(),
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

fn api_key(org_id: Uuid) -> m::ApiKey {
    let now = Utc::now();
    m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org_id,
        value: KEY.into(),
        name: Some("test".into()),
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn monthly_plan(org_id: Uuid, name: &str, amount_cents: i64) -> m::Plan {
    let now = Utc::now();
    m::Plan {
        id: Uuid::new_v4(),
        organization_id: org_id,
        name: name.into(),
        code: format!("{}-{}", name.to_lowercase(), &Uuid::new_v4().to_string()[..8]),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(amount_cents),
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

fn active_subscription(org_id: Uuid, customer_id: Uuid, plan_id: Uuid) -> m::Subscription {
    let now = Utc::now();
    let start = now - Duration::days(40);
    m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org_id,
        customer_id,
        plan_id,
        billing_entity_id: Some(org_id),
        external_id: format!("sub-{}", &Uuid::new_v4().to_string()[..8]),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(start),
        started_at: Some(start),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    }
}

async fn delete(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {KEY}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn status_of(db: &Db, id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT status FROM subscriptions WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn invoices_of(db: &Db, org: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE organization_id = $1")
        .bind(org)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn downgrade_is_canceled_only_after_the_final_invoice_succeeds(pool: PgPool) {
    let db = Db::new(pool);
    let org = org();
    db.insert_organization(&org).await.unwrap();
    db.insert_api_key(&api_key(org.id)).await.unwrap();
    let cust = customer(org.id);
    db.insert_customer(&cust).await.unwrap();
    let pro = monthly_plan(org.id, "Pro", 2_900);
    let basic = monthly_plan(org.id, "Basic", 900);
    db.insert_plan(&pro).await.unwrap();
    db.insert_plan(&basic).await.unwrap();
    let sub = active_subscription(org.id, cust.id, pro.id);
    db.insert_subscription(&sub).await.unwrap();

    let st = AppState::new(db.clone());
    let app = rago_api::router(st.clone());

    // Pro -> Basic, scheduled for the next billing date.
    let pending = rago_api::plan_change::schedule(&st, org.id, &sub, &basic).await.unwrap();
    assert_eq!(status_of(&db, pending.id).await, SubscriptionStatus::Pending.as_i32());
    let invoices_before = invoices_of(&db, org.id).await;

    // Attempt 1: the final invoice cannot be written.
    sqlx::raw_sql(
        "CREATE FUNCTION test_refuse_invoice() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'injected final invoice failure'; END $$; \
         CREATE TRIGGER test_refuse_invoice BEFORE INSERT ON invoices \
         FOR EACH ROW EXECUTE FUNCTION test_refuse_invoice();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let uri = format!("/api/v1/subscriptions/{}", sub.external_id);
    let (status, _) = delete(&app, &uri).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "a failed final invoice fails the termination");
    assert_eq!(status_of(&db, sub.id).await, SubscriptionStatus::Active.as_i32(), "still active");
    assert_eq!(
        status_of(&db, pending.id).await,
        SubscriptionStatus::Pending.as_i32(),
        "the scheduled downgrade survives a failed final invoice"
    );
    let still_scheduled = db.scheduled_downgrade_for(sub.id).await.unwrap();
    assert_eq!(still_scheduled.map(|s| s.id), Some(pending.id));
    assert_eq!(invoices_of(&db, org.id).await, invoices_before, "nothing half-written");

    // Attempt 2 (retry): the final invoice is issued, then the downgrade goes.
    sqlx::raw_sql("DROP TRIGGER test_refuse_invoice ON invoices; DROP FUNCTION test_refuse_invoice();")
        .execute(&db.pool)
        .await
        .unwrap();
    let (status, body) = delete(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["subscription"]["status"], "terminated");
    assert_eq!(status_of(&db, sub.id).await, SubscriptionStatus::Terminated.as_i32());
    assert_eq!(status_of(&db, pending.id).await, SubscriptionStatus::Canceled.as_i32());
    assert!(db.scheduled_downgrade_for(sub.id).await.unwrap().is_none());
    assert!(invoices_of(&db, org.id).await > invoices_before, "the final invoice was issued");
}

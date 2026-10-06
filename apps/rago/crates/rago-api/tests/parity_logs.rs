// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set.
#![cfg(feature = "pg-tests")]

//! Lago REST parity: instant-fee estimates, enriched events, API and
//! security logs, activity log show, usage analytics, gRPC token, document
//! requests and the flat quote-version routes.

mod parity;

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

/// A sum metric `tokens` on a pay-in-advance standard charge at 0.5 per unit.
async fn instant_setup(a: &parity::Api) {
    a.customer("cust-i").await;
    a.ok("POST", "/api/v1/billable_metrics", Some(json!({ "billable_metric": { "name": "Tokens", "code": "tokens", "aggregation_type": "sum_agg", "field_name": "tokens" } }))).await;
    a.ok(
        "POST",
        "/api/v1/plans",
        Some(json!({ "plan": { "name": "Instant", "code": "inst", "interval": "monthly", "amount_cents": 0, "amount_currency": "USD",
            "charges": [{ "billable_metric_code": "tokens", "charge_model": "standard", "pay_in_advance": true, "properties": { "amount": "0.5" } }] } })),
    )
    .await;
    a.ok("POST", "/api/v1/subscriptions", Some(json!({ "subscription": { "external_customer_id": "cust-i", "plan_code": "inst", "external_id": "sub-i" } }))).await;
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn instant_fee_estimates_price_without_storing(pool: PgPool) {
    let a = parity::api(pool).await;
    instant_setup(&a).await;
    let one = a
        .ok("POST", "/api/v1/events/estimate_instant_fees", Some(json!({ "event": { "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 7 } } })))
        .await;
    let fee = &one["fees"][0];
    assert_eq!(fee["amount_cents"], 350, "{one}");
    assert_eq!(fee["units"], "7");
    assert_eq!(fee["item"]["code"], "tokens");
    let batch = a
        .ok(
            "POST",
            "/api/v1/events/batch_estimate_instant_fees",
            Some(json!({ "events": [
                { "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 2 } },
                { "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 4 } },
            ] })),
        )
        .await;
    let cents: Vec<i64> = batch["fees"].as_array().unwrap().iter().map(|f| f["amount_cents"].as_i64().unwrap()).collect();
    assert_eq!(cents, vec![100, 200]);
    // Mixed subscriptions and unknown codes are refused.
    let (st, _) = a
        .call("POST", "/api/v1/events/batch_estimate_instant_fees", Some(json!({ "events": [
            { "code": "tokens", "external_subscription_id": "sub-i" },
            { "code": "tokens", "external_subscription_id": "other" },
        ] })))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = a
        .call("POST", "/api/v1/events/estimate_instant_fees", Some(json!({ "event": { "code": "nope", "external_subscription_id": "sub-i" } })))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    // Nothing was stored.
    let enriched = a.ok("GET", "/api/v1/events_enriched?external_subscription_id=sub-i", None).await;
    assert!(enriched["events"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn enriched_events_show_how_charges_read_them(pool: PgPool) {
    let a = parity::api(pool).await;
    instant_setup(&a).await;
    a.ok("POST", "/api/v1/events", Some(json!({ "event": { "transaction_id": "t1", "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 4 } } }))).await;
    let v = a.ok("GET", "/api/v1/events_enriched?external_subscription_id=sub-i&code=tokens", None).await;
    let e = &v["events"][0];
    assert_eq!(e["transaction_id"], "t1", "{v}");
    assert_eq!(e["value"], "4");
    assert!(e["lago_charge_id"].is_string());
    assert_eq!(v["meta"]["total_count"], 1);
    assert_eq!(a.call("GET", "/api/v1/events_enriched", None).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn api_and_security_logs_record_mutations_with_secrets_masked(pool: PgPool) {
    let a = parity::api(pool).await;
    a.ok("POST", "/api/v1/customers", Some(json!({ "customer": { "external_id": "c-log", "name": "Log", "metadata_password": "hunter2" } }))).await;
    a.ok("GET", "/api/v1/customers", None).await;
    let logs = a.ok("GET", "/api/v1/api_logs", None).await;
    let rows = logs["api_logs"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "only mutations are logged: {logs}");
    assert_eq!(rows[0]["http_method"], "POST");
    assert_eq!(rows[0]["http_status"], 200);
    assert_eq!(rows[0]["request_path"], "/api/v1/customers");
    assert_eq!(rows[0]["request_body"]["customer"]["metadata_password"], "[FILTERED]");
    assert_eq!(rows[0]["request_body"]["customer"]["name"], "Log");
    let id = rows[0]["request_id"].as_str().unwrap();
    assert_eq!(a.ok("GET", &format!("/api/v1/api_logs/{id}"), None).await["api_log"]["request_id"], id);
    // Another org sees neither the list entries nor the row.
    let other = a.other_org_token().await;
    assert_eq!(a.call_as(&other, "GET", &format!("/api/v1/api_logs/{id}"), None).await.0, StatusCode::NOT_FOUND);

    // Webhook endpoint changes land in the security log.
    let made = a.ok("POST", "/api/v1/webhook_endpoints", Some(json!({ "webhook_endpoint": { "webhook_url": "https://hooks.example/a" } }))).await;
    assert_eq!(made["webhook_endpoint"]["signature_algo"], "hmac", "the stored algo is reported");
    let wid = made["webhook_endpoint"]["lago_id"].as_str().unwrap();
    a.ok("DELETE", &format!("/api/v1/webhook_endpoints/{wid}"), None).await;
    let sec = a.ok("GET", "/api/v1/security_logs", None).await;
    let events: Vec<&str> = sec["security_logs"].as_array().unwrap().iter().map(|l| l["log_event"].as_str().unwrap()).collect();
    assert_eq!(events, vec!["webhook_endpoint.deleted", "webhook_endpoint.created"]);
    let lid = sec["security_logs"][0]["log_id"].as_str().unwrap();
    assert_eq!(a.ok("GET", &format!("/api/v1/security_logs/{lid}"), None).await["security_log"]["log_type"], "webhook_endpoint");
    assert_eq!(a.call_as(&other, "GET", &format!("/api/v1/security_logs/{lid}"), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn webhook_signature_algo_is_validated(pool: PgPool) {
    let a = parity::api(pool).await;
    let (st, v) = a
        .call("POST", "/api/v1/webhook_endpoints", Some(json!({ "webhook_endpoint": { "webhook_url": "https://hooks.example/b", "signature_algo": "md5" } })))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    // No RSA key in the test environment: jwt is refused, the public key 404s.
    if std::env::var("RAGO_RSA_PRIVATE_KEY").is_err() {
        let (st, _) = a
            .call("POST", "/api/v1/webhook_endpoints", Some(json!({ "webhook_endpoint": { "webhook_url": "https://hooks.example/b", "signature_algo": "jwt" } })))
            .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(a.call("GET", "/api/v1/webhooks/json_public_key", None).await.0, StatusCode::NOT_FOUND);
    }
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn analytics_usage_grpc_token_and_activity_log(pool: PgPool) {
    let a = parity::api(pool.clone()).await;
    let today = chrono::Utc::now().date_naive();
    sqlx::query(
        "INSERT INTO daily_usages (organization_id, subscription_external_id, usage_date, amount_cents, refreshed_at, created_at) VALUES ($1,'s1',$2,250,now(),now())",
    )
    .bind(a.org.id)
    .bind(today)
    .execute(&pool)
    .await
    .expect("daily usage row");
    let v = a.ok("GET", "/api/v1/analytics/usage", None).await;
    assert_eq!(v["usages"][0]["amount_cents"], 250, "{v}");
    assert_eq!(v["usages"][0]["external_subscription_id"], "s1");
    assert_eq!(a.call("GET", "/api/v1/analytics/usage?from_date=2026-02-01&to_date=2026-01-01", None).await.0, StatusCode::UNPROCESSABLE_ENTITY);

    let t = a.ok("GET", "/api/v1/organizations/grpc_token", None).await;
    let token = t["organization"]["grpc_token"].as_str().unwrap();
    assert_eq!(token.split('.').count(), 3);
    // It is not a dashboard session.
    assert_eq!(a.call_as(token, "GET", "/api/v1/customers", None).await.0, StatusCode::UNAUTHORIZED);

    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO activity_logs (id, organization_id, activity_type, resource_type, resource_id, payload, created_at) VALUES ($1,$2,'customer.created','Customer',$3,'{}',now())")
        .bind(id)
        .bind(a.org.id)
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("activity row");
    let log = a.ok("GET", &format!("/api/v1/activity_logs/{id}"), None).await;
    assert_eq!(log["activity_log"]["activity_id"], id.to_string(), "{log}");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn document_requests_and_flat_quote_versions(pool: PgPool) {
    let a = parity::api(pool).await;
    a.customer("cust-q").await;
    let q = a
        .ok("POST", "/api/v1/quotes", Some(json!({ "quote": { "external_customer_id": "cust-q", "line_items": [{ "name": "Setup", "unit_amount_cents": 5000, "units": 1 }] } })))
        .await;
    let qid = q["quote"]["lago_id"].as_str().unwrap().to_string();
    let versions = a.ok("GET", &format!("/api/v1/quotes/{qid}/versions"), None).await;
    let v1 = versions["quote_versions"][0]["lago_id"].as_str().unwrap().to_string();
    assert_eq!(a.ok("GET", &format!("/api/v1/quote_versions/{v1}"), None).await["quote_version"]["version_number"], 1);
    let approved = a.ok("POST", &format!("/api/v1/quote_versions/{v1}/approve"), None).await;
    assert_eq!(approved["quote_version"]["status"], "approved", "{approved}");
    let cloned = a.ok("POST", &format!("/api/v1/quote_versions/{v1}/clone"), None).await;
    let v2 = cloned["quote_version"]["lago_id"].as_str().unwrap().to_string();
    assert_eq!(cloned["quote_version"]["status"], "draft");
    assert_eq!(cloned["quote_version"]["version_number"], 2);
    assert_eq!(a.ok("POST", &format!("/api/v1/quote_versions/{v2}/void"), None).await["quote_version"]["status"], "voided");
    let other = a.other_org_token().await;
    assert_eq!(a.call_as(&other, "GET", &format!("/api/v1/quote_versions/{v1}"), None).await.0, StatusCode::NOT_FOUND);

    // An invoice: document requests answer with the download URLs.
    let inv = a
        .ok("POST", "/api/v1/invoices", Some(json!({ "invoice": { "external_customer_id": "cust-q", "currency": "USD", "fees": [{ "name": "One-off", "unit_amount_cents": 1200, "units": 1 }] } })))
        .await;
    let iid = inv["invoice"]["lago_id"].as_str().unwrap();
    let pdf = a.ok("POST", &format!("/api/v1/invoices/{iid}/download_pdf"), None).await;
    assert!(pdf["invoice"]["file_url"].as_str().unwrap().ends_with(&format!("/api/v1/invoices/{iid}/download")));
    let xml = a.ok("POST", &format!("/api/v1/invoices/{iid}/download_xml"), None).await;
    assert!(xml["invoice"]["xml_url"].as_str().unwrap().ends_with("/download.xml"));
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn v1_events_batch_and_post_download_aliases(pool: PgPool) {
    let a = parity::api(pool).await;
    instant_setup(&a).await;
    a.ok(
        "POST",
        "/api/v1/events/batch",
        Some(json!({ "events": [
            { "transaction_id": "b1", "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 1 } },
            { "transaction_id": "b2", "code": "tokens", "external_subscription_id": "sub-i", "properties": { "tokens": 2 } },
        ] })),
    )
    .await;
    let v = a.ok("GET", "/api/v1/events_enriched?external_subscription_id=sub-i", None).await;
    assert_eq!(v["meta"]["total_count"], 2, "{v}");
    let inv = a
        .ok("POST", "/api/v1/invoices", Some(json!({ "invoice": { "external_customer_id": "cust-i", "currency": "USD", "fees": [{ "name": "X", "unit_amount_cents": 100, "units": 1 }] } })))
        .await;
    let iid = inv["invoice"]["lago_id"].as_str().unwrap();
    assert!(a.ok("POST", &format!("/api/v1/invoices/{iid}/download"), None).await["invoice"]["file_url"].is_string());
}

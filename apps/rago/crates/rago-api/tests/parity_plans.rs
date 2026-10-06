// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set.
#![cfg(feature = "pg-tests")]

//! Lago REST parity: plan charge filters, subscription charge and fixed
//! charge overrides (the plan and other subscribers untouched), entitlements
//! on plans and subscriptions, feature privilege removal, subscription
//! alerts destroy-all and lifetime usage with carried-in usage.

mod parity;

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::PgPool;

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn plan_charge_filters_one_by_one(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-f").await;
    a.plan_and_subscription(&c, "pro", "sub-f").await;
    let base = "/api/v1/plans/pro/charges/calls_pro/filters";
    let made = a.ok("POST", base, Some(json!({ "filter": { "invoice_display_name": "EU", "properties": { "amount": "3" }, "values": { "region": ["eu"] } } }))).await;
    let id = made["filter"]["lago_id"].as_str().unwrap().to_string();
    assert_eq!(made["filter"]["values"], json!({ "region": ["eu"] }));
    assert_eq!(a.ok("GET", base, None).await["filters"].as_array().unwrap().len(), 1);
    let up = a
        .ok(
            "PUT",
            &format!("{base}/{id}"),
            Some(json!({ "filter": { "properties": { "amount": "4" } } })),
        )
        .await;
    assert_eq!(up["filter"]["properties"], json!({ "amount": "4" }));
    assert_eq!(up["filter"]["invoice_display_name"], "EU", "unchanged fields stay");
    assert_eq!(
        a.call("POST", base, Some(json!({ "filter": { "values": {} } })))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    a.ok("DELETE", &format!("{base}/{id}"), None).await;
    assert_eq!(
        a.call("GET", &format!("{base}/{id}"), None).await.0,
        StatusCode::NOT_FOUND
    );
    // The plan's charge shows its filters too.
    assert!(
        a.ok("GET", "/api/v1/plans/pro/charges/calls_pro", None).await["charge"]["filters"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn a_subscription_override_changes_only_that_subscription(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-o").await;
    let (plan, sub) = a.plan_and_subscription(&c, "team", "sub-o").await;
    // A second subscriber of the same plan.
    let c2 = a.customer("cust-o2").await;
    let mut other = sub.clone();
    other.id = uuid::Uuid::new_v4();
    other.customer_id = c2.id;
    other.external_id = "sub-o2".into();
    a.db.insert_subscription(&other).await.unwrap();
    a.ok(
        "POST",
        "/api/v1/add_ons",
        Some(json!({ "add_on": { "name": "Seat", "code": "seat", "amount_cents": 500, "amount_currency": "USD" } })),
    )
    .await;
    a.ok(
        "POST",
        "/api/v1/plans/team/fixed_charges",
        Some(json!({ "fixed_charge": { "add_on_code": "seat", "units": "2", "properties": { "amount": "5" } } })),
    )
    .await;

    let listed = a.ok("GET", "/api/v1/subscriptions/sub-o/charges", None).await;
    assert_eq!(
        listed["charges"][0]["lago_parent_id"],
        Value::Null,
        "reads do not override"
    );
    let updated = a
        .ok("PUT", "/api/v1/subscriptions/sub-o/charges/calls_team", Some(json!({ "charge": { "properties": { "amount": "2" }, "invoice_display_name": "Calls (custom)", "filters": [{ "properties": { "amount": "9" }, "values": { "region": ["eu"] } }] } })))
        .await;
    assert_eq!(updated["charge"]["properties"], json!({ "amount": "2" }));
    assert_eq!(updated["charge"]["filters"].as_array().unwrap().len(), 1);
    let parent = updated["charge"]["lago_parent_id"].as_str().unwrap().to_string();
    // The plan and the other subscriber keep the original price.
    let original = a.ok("GET", "/api/v1/plans/team/charges/calls_team", None).await;
    assert_eq!(original["charge"]["properties"], json!({ "amount": "1" }));
    assert_eq!(original["charge"]["lago_id"].as_str().unwrap(), parent);
    assert_eq!(
        a.ok("GET", "/api/v1/subscriptions/sub-o2/charges/calls_team", None)
            .await["charge"]["properties"],
        json!({ "amount": "1" })
    );
    // The subscription now has its own plan copy, which still answers to the code.
    let now_sub =
        a.db.find_subscription_by_external_id(a.org.id, "sub-o")
            .await
            .unwrap()
            .unwrap();
    assert_ne!(now_sub.plan_id, plan.id);
    assert_eq!(
        a.db.find_plan_by_code(a.org.id, "team").await.unwrap().unwrap().id,
        plan.id,
        "the code still names the parent plan"
    );
    let plans = a.ok("GET", "/api/v1/plans", None).await;
    assert_eq!(
        plans["plans"].as_array().unwrap().len(),
        1,
        "override plans are not listed"
    );

    // Filters of the subscription charge.
    let fid = a
        .ok("GET", "/api/v1/subscriptions/sub-o/charges/calls_team/filters", None)
        .await["filters"][0]["lago_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(
        "PUT",
        &format!("/api/v1/subscriptions/sub-o/charges/calls_team/filters/{fid}"),
        Some(json!({ "filter": { "properties": { "amount": "8" } } })),
    )
    .await;
    a.ok(
        "POST",
        "/api/v1/subscriptions/sub-o/charges/calls_team/filters",
        Some(json!({ "filter": { "values": { "region": ["us"] } } })),
    )
    .await;
    assert_eq!(
        a.ok("GET", "/api/v1/subscriptions/sub-o/charges/calls_team/filters", None)
            .await["filters"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    a.ok(
        "DELETE",
        &format!("/api/v1/subscriptions/sub-o/charges/calls_team/filters/{fid}"),
        None,
    )
    .await;

    // Fixed charge units for this subscription only.
    let fc = a
        .ok(
            "PUT",
            "/api/v1/subscriptions/sub-o/fixed_charges/seat",
            Some(json!({ "fixed_charge": { "units": "7" } })),
        )
        .await;
    assert_eq!(fc["fixed_charge"]["units"], "7");
    assert_eq!(
        a.ok("GET", "/api/v1/subscriptions/sub-o2/fixed_charges/seat", None)
            .await["fixed_charge"]["units"],
        "2"
    );
    assert_eq!(
        a.ok("GET", "/api/v1/subscriptions/sub-o/fixed_charges", None).await["fixed_charges"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        a.call(
            "PUT",
            "/api/v1/subscriptions/sub-o/fixed_charges/nope",
            Some(json!({ "fixed_charge": { "units": "1" } }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // The invoice preview bills the overridden price: 7 seats at $5 + the $10 fee.
    let preview = a
        .ok(
            "POST",
            "/api/v1/invoices/preview",
            Some(json!({ "subscriptions": { "external_ids": ["sub-o"] } })),
        )
        .await;
    assert_eq!(preview["invoice"]["total_amount_cents"], 4_500, "{preview}");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn entitlements_on_plans_and_subscriptions(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-e").await;
    a.plan_and_subscription(&c, "biz", "sub-e").await;
    // Features with privileges.
    a.ok("POST", "/api/v1/entitlements", Some(json!({ "feature_code": "seats", "plan_code": "biz", "privileges": [{ "code": "max", "value_type": "integer" }, { "code": "sso", "value_type": "boolean" }], "values": { "max": 5 } }))).await;
    a.ok("POST", "/api/v1/entitlements", Some(json!({ "feature_code": "api", "plan_code": "biz", "privileges": [{ "code": "rpm", "value_type": "integer" }], "values": { "rpm": 100 } }))).await;

    let merged = a
        .ok(
            "PATCH",
            "/api/v1/plans/biz/entitlements",
            Some(json!({ "entitlements": { "seats": { "max": 10, "sso": true } } })),
        )
        .await;
    let seats = merged["entitlements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "seats")
        .unwrap()
        .clone();
    assert_eq!(seats["privileges"].as_array().unwrap().len(), 2);
    assert!(seats["privileges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["code"] == "max" && p["value"] == 10));
    assert_eq!(
        merged["entitlements"].as_array().unwrap().len(),
        2,
        "patch keeps the other features"
    );
    assert_eq!(
        a.call(
            "PATCH",
            "/api/v1/plans/biz/entitlements",
            Some(json!({ "entitlements": { "seats": { "max": "many" } } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        a.call(
            "PATCH",
            "/api/v1/plans/biz/entitlements",
            Some(json!({ "entitlements": { "nope": {} } }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // The subscription inherits, overrides and removes.
    let sub = a
        .ok(
            "PATCH",
            "/api/v1/subscriptions/sub-e/entitlements",
            Some(json!({ "entitlements": { "seats": { "max": 50 } } })),
        )
        .await;
    let seats = sub["entitlements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "seats")
        .unwrap()
        .clone();
    assert!(seats["privileges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["code"] == "max" && p["value"] == 50));
    a.ok("DELETE", "/api/v1/subscriptions/sub-e/entitlements/api", None)
        .await;
    let resolved = a.ok("GET", "/api/v1/subscriptions/sub-e/entitlements", None).await;
    assert!(
        resolved["entitlements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["code"] != "api"),
        "{resolved}"
    );
    // The plan still has it.
    assert_eq!(
        a.ok("GET", "/api/v1/plans/biz/entitlements/api", None).await["entitlement"]["code"],
        "api"
    );
    // Setting it again on the subscription brings it back.
    a.ok(
        "PATCH",
        "/api/v1/subscriptions/sub-e/entitlements",
        Some(json!({ "entitlements": { "api": { "rpm": 1000 } } })),
    )
    .await;
    let resolved = a.ok("GET", "/api/v1/subscriptions/sub-e/entitlements", None).await;
    assert!(resolved["entitlements"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["code"] == "api"));
    a.ok(
        "DELETE",
        "/api/v1/subscriptions/sub-e/entitlements/seats/privileges/sso",
        None,
    )
    .await;

    // POST replaces the plan's entitlements; privilege and feature removal.
    let replaced = a
        .ok(
            "POST",
            "/api/v1/plans/biz/entitlements",
            Some(json!({ "entitlements": { "seats": { "max": 3 } } })),
        )
        .await;
    assert_eq!(replaced["entitlements"].as_array().unwrap().len(), 1);
    assert_eq!(replaced["entitlements"][0]["privileges"].as_array().unwrap().len(), 1);
    a.ok("DELETE", "/api/v1/plans/biz/entitlements/seats/privileges/max", None)
        .await;
    a.ok("DELETE", "/api/v1/plans/biz/entitlements/seats", None).await;
    assert_eq!(
        a.call("GET", "/api/v1/plans/biz/entitlements/seats", None).await.0,
        StatusCode::NOT_FOUND
    );
    a.ok("DELETE", "/api/v1/features/seats/privileges/sso", None).await;
    assert_eq!(
        a.call("DELETE", "/api/v1/features/seats/privileges/sso", None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn subscription_alerts_and_lifetime_usage(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-l").await;
    a.plan_and_subscription(&c, "lt", "sub-l").await;
    for code in ["a1", "a2"] {
        a.ok("POST", "/api/v1/subscriptions/sub-l/alerts", Some(json!({ "alert": { "alert_type": "current_usage_amount", "code": code, "thresholds": [{ "value": "100" }] } }))).await;
    }
    assert_eq!(
        a.ok("DELETE", "/api/v1/subscriptions/sub-l/alerts", None).await["alerts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(a.ok("GET", "/api/v1/subscriptions/sub-l/alerts", None).await["alerts"]
        .as_array()
        .unwrap()
        .is_empty());

    let lu = a
        .ok(
            "PUT",
            "/api/v1/subscriptions/sub-l/lifetime_usage",
            Some(json!({ "lifetime_usage": { "external_historical_usage_amount_cents": 2500 } })),
        )
        .await;
    assert_eq!(lu["lifetime_usage"]["external_historical_usage_amount_cents"], 2500);
    assert_eq!(lu["lifetime_usage"]["total_usage_amount_cents"], 2500);
    assert_eq!(
        a.ok("GET", "/api/v1/subscriptions/sub-l/lifetime_usage", None).await["lifetime_usage"]
            ["total_usage_amount_cents"],
        2500
    );
    assert_eq!(
        a.call(
            "PUT",
            "/api/v1/subscriptions/sub-l/lifetime_usage",
            Some(json!({ "lifetime_usage": { "external_historical_usage_amount_cents": -1 } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

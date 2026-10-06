// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set.
#![cfg(feature = "pg-tests")]

//! Lago REST parity: customer wallets by code (with alerts and metadata),
//! wallet, plan and credit-note metadata, payment methods, and wallet
//! transactions (show, fundings, consumptions, the wallet's list).

mod parity;

use axum::http::StatusCode;
use chrono::Utc;
use rago_core::enums::{WalletTransactionStatus, WalletTransactionType};
use rago_core::models::WalletTransaction;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn customer_wallets_are_addressed_by_code(pool: PgPool) {
    let a = parity::api(pool).await;
    a.customer("cust-1").await;
    let w = a.ok("POST", "/api/v1/customers/cust-1/wallets", Some(json!({ "wallet": { "name": "Prepaid credits", "rate_amount": "1", "currency": "USD", "metadata": { "team": "growth" } } }))).await;
    assert_eq!(w["wallet"]["code"], "prepaid_credits");
    assert_eq!(w["wallet"]["external_customer_id"], "cust-1");
    assert_eq!(w["wallet"]["metadata"], json!({ "team": "growth" }));
    // Same name: the derived code gets a suffix; a taken explicit code is refused.
    let w2 = a
        .ok(
            "POST",
            "/api/v1/customers/cust-1/wallets",
            Some(json!({ "wallet": { "name": "Prepaid credits", "rate_amount": "1" } })),
        )
        .await;
    assert_eq!(w2["wallet"]["code"], "prepaid_credits_2");
    let (status, _) = a
        .call(
            "POST",
            "/api/v1/customers/cust-1/wallets",
            Some(json!({ "wallet": { "code": "prepaid_credits", "rate_amount": "1" } })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let list = a.ok("GET", "/api/v1/customers/cust-1/wallets", None).await;
    assert_eq!(list["wallets"].as_array().unwrap().len(), 2);
    let shown = a
        .ok("GET", "/api/v1/customers/cust-1/wallets/prepaid_credits", None)
        .await;
    assert_eq!(shown["wallet"]["lago_id"], w["wallet"]["lago_id"]);
    let updated = a
        .ok(
            "PUT",
            "/api/v1/customers/cust-1/wallets/prepaid_credits",
            Some(json!({ "wallet": { "name": "Credits", "metadata": { "team": "sales" } } })),
        )
        .await;
    assert_eq!(updated["wallet"]["name"], "Credits");
    assert_eq!(updated["wallet"]["metadata"], json!({ "team": "sales" }));
    assert_eq!(
        a.call("GET", "/api/v1/customers/cust-1/wallets/nope", None).await.0,
        StatusCode::NOT_FOUND
    );

    // Alerts on the wallet.
    let alert = a
        .ok("POST", "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts", Some(json!({ "alert": { "alert_type": "wallet_balance_amount", "code": "low", "thresholds": [{ "value": "500" }] } })))
        .await;
    assert_eq!(alert["alert"]["code"], "low");
    assert_eq!(
        a.call(
            "POST",
            "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts",
            Some(json!({ "alert": { "alert_type": "nope", "code": "x", "thresholds": [{ "value": "1" }] } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    a.ok(
        "PUT",
        "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts/low",
        Some(json!({ "alert": { "name": "Low balance" } })),
    )
    .await;
    assert_eq!(
        a.ok(
            "GET",
            "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts/low",
            None
        )
        .await["alert"]["name"],
        "Low balance"
    );
    assert_eq!(
        a.ok("GET", "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts", None)
            .await["alerts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    a.ok(
        "DELETE",
        "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts",
        None,
    )
    .await;
    assert!(a
        .ok("GET", "/api/v1/customers/cust-1/wallets/prepaid_credits/alerts", None)
        .await["alerts"]
        .as_array()
        .unwrap()
        .is_empty());

    // Metadata through the code path, then terminate.
    a.ok(
        "PUT",
        "/api/v1/customers/cust-1/wallets/prepaid_credits/metadata",
        Some(json!({ "metadata": { "region": "eu" } })),
    )
    .await;
    let m = a
        .ok(
            "DELETE",
            "/api/v1/customers/cust-1/wallets/prepaid_credits/metadata/team",
            None,
        )
        .await;
    assert_eq!(m["metadata"], json!({ "region": "eu" }));
    let gone = a
        .ok("DELETE", "/api/v1/customers/cust-1/wallets/prepaid_credits", None)
        .await;
    assert_eq!(gone["wallet"]["status"], "terminated");
    // The code is free again for an active wallet.
    a.ok(
        "POST",
        "/api/v1/customers/cust-1/wallets",
        Some(json!({ "wallet": { "code": "prepaid_credits", "rate_amount": "1" } })),
    )
    .await;

    // Another organization never sees this customer.
    let other = a.other_org_token().await;
    assert_eq!(
        a.call_as(&other, "GET", "/api/v1/customers/cust-1/wallets", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn metadata_create_replaces_update_merges_and_keys_are_removed(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-m").await;
    a.plan_and_subscription(&c, "pro", "sub-m").await;
    let set = a
        .ok(
            "POST",
            "/api/v1/plans/pro/metadata",
            Some(json!({ "metadata": { "tier": "gold", "seats": 5 } })),
        )
        .await;
    assert_eq!(set["metadata"], json!({ "tier": "gold", "seats": "5" }));
    let merged = a
        .ok(
            "PUT",
            "/api/v1/plans/pro/metadata",
            Some(json!({ "metadata": { "owner": "ops" } })),
        )
        .await;
    assert_eq!(
        merged["metadata"],
        json!({ "tier": "gold", "seats": "5", "owner": "ops" })
    );
    let replaced = a
        .ok(
            "POST",
            "/api/v1/plans/pro/metadata",
            Some(json!({ "metadata": { "only": "this" } })),
        )
        .await;
    assert_eq!(replaced["metadata"], json!({ "only": "this" }));
    assert_eq!(
        a.ok("DELETE", "/api/v1/plans/pro/metadata/only", None).await["metadata"],
        Value::Null
    );
    assert_eq!(
        a.call(
            "POST",
            "/api/v1/plans/pro/metadata",
            Some(json!({ "metadata": { "x": { "deep": 1 } } }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let too_many: serde_json::Map<String, Value> = (0..51).map(|i| (format!("k{i}"), json!("v"))).collect();
    assert_eq!(
        a.call(
            "POST",
            "/api/v1/plans/pro/metadata",
            Some(json!({ "metadata": too_many }))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        a.call("POST", "/api/v1/plans/nope/metadata", Some(json!({ "metadata": {} })))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Wallet metadata by id.
    let w = a
        .ok(
            "POST",
            "/api/v1/wallets",
            Some(json!({ "wallet": { "external_customer_id": "cust-m", "rate_amount": "1" } })),
        )
        .await;
    let wid = w["wallet"]["lago_id"].as_str().unwrap().to_string();
    a.ok(
        "POST",
        &format!("/api/v1/wallets/{wid}/metadata"),
        Some(json!({ "metadata": { "a": "1" } })),
    )
    .await;
    assert_eq!(
        a.ok("DELETE", &format!("/api/v1/wallets/{wid}/metadata"), None).await["metadata"],
        Value::Null
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn payment_methods_are_removed_and_made_default(pool: PgPool) {
    let a = parity::api(pool).await;
    let c = a.customer("cust-pm").await;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    a.db.insert_payment_method(first, a.org.id, c.id, Some("pm_1"), true)
        .await
        .unwrap();
    a.db.insert_payment_method(second, a.org.id, c.id, Some("pm_2"), false)
        .await
        .unwrap();
    let d = a
        .ok(
            "PUT",
            &format!("/api/v1/customers/cust-pm/payment_methods/{second}/set_as_default"),
            None,
        )
        .await;
    assert_eq!(d["payment_method"]["is_default"], true);
    let list = a.ok("GET", "/api/v1/customers/cust-pm/payment_methods", None).await;
    let defaults: Vec<&Value> = list["payment_methods"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["is_default"] == true)
        .collect();
    assert_eq!(defaults.len(), 1);
    assert_eq!(defaults[0]["provider_method_id"], "pm_2");
    a.ok(
        "DELETE",
        &format!("/api/v1/customers/cust-pm/payment_methods/{first}"),
        None,
    )
    .await;
    assert_eq!(
        a.ok("GET", "/api/v1/customers/cust-pm/payment_methods", None).await["payment_methods"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Another customer's method is not reachable through this customer.
    a.customer("cust-other").await;
    assert_eq!(
        a.call(
            "DELETE",
            &format!("/api/v1/customers/cust-other/payment_methods/{second}"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn wallet_transactions_show_their_fundings_and_consumptions(pool: PgPool) {
    let a = parity::api(pool).await;
    a.customer("cust-t").await;
    let w = a
        .ok(
            "POST",
            "/api/v1/wallets",
            Some(json!({ "wallet": { "external_customer_id": "cust-t", "rate_amount": "1" } })),
        )
        .await;
    let wid: Uuid = w["wallet"]["lago_id"].as_str().unwrap().parse().unwrap();
    let t1 = a
        .ok(
            "POST",
            "/api/v1/wallet_transactions",
            Some(json!({ "wallet_transaction": { "wallet_id": wid, "granted_credits": "10" } })),
        )
        .await;
    let t2 = a
        .ok(
            "POST",
            "/api/v1/wallet_transactions",
            Some(json!({ "wallet_transaction": { "wallet_id": wid, "granted_credits": "5" } })),
        )
        .await;
    let in1 = t1["wallet_transactions"][0]["lago_id"].as_str().unwrap().to_string();
    let in2 = t2["wallet_transactions"][0]["lago_id"].as_str().unwrap().to_string();
    // An invoice drew 12 credits.
    let out = Uuid::new_v4();
    let now = Utc::now() + chrono::Duration::seconds(5);
    a.db.insert_wallet_transaction(&WalletTransaction {
        id: out,
        organization_id: a.org.id,
        wallet_id: wid,
        transaction_type: WalletTransactionType::Outbound,
        status: WalletTransactionStatus::Settled,
        amount: Decimal::from(12),
        credit_amount: Decimal::from(12),
        invoice_id: None,
        settled_at: Some(now),
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();

    let shown = a.ok("GET", &format!("/api/v1/wallet_transactions/{in1}"), None).await;
    assert_eq!(shown["wallet_transaction"]["transaction_type"], "inbound");
    let fundings = a
        .ok("GET", &format!("/api/v1/wallet_transactions/{out}/fundings"), None)
        .await;
    let f = fundings["wallet_transaction_fundings"].as_array().unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(
        (
            f[0]["lago_inbound_wallet_transaction_id"].as_str().unwrap(),
            f[0]["consumed_credits"].as_str().unwrap()
        ),
        (in1.as_str(), "10")
    );
    assert_eq!(
        (
            f[1]["lago_inbound_wallet_transaction_id"].as_str().unwrap(),
            f[1]["consumed_credits"].as_str().unwrap()
        ),
        (in2.as_str(), "2")
    );
    let c = a
        .ok("GET", &format!("/api/v1/wallet_transactions/{in2}/consumptions"), None)
        .await;
    assert_eq!(c["wallet_transaction_consumptions"][0]["consumed_credits"], "2");
    assert_eq!(
        a.call("GET", &format!("/api/v1/wallet_transactions/{out}/consumptions"), None)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // A granted (settled) transaction has no payment to make.
    assert_eq!(
        a.call("POST", &format!("/api/v1/wallet_transactions/{in1}/payment_url"), None)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let listed = a
        .ok("GET", &format!("/api/v1/wallets/{wid}/wallet_transactions"), None)
        .await;
    assert_eq!(listed["wallet_transactions"].as_array().unwrap().len(), 3);
    let other = a.other_org_token().await;
    assert_eq!(
        a.call_as(&other, "GET", &format!("/api/v1/wallet_transactions/{in1}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

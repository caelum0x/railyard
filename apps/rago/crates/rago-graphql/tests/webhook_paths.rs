// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! The GraphQL customer and wallet mutations emit their webhooks
//! (`customer.created`, `customer.updated`, `wallet.created`), like the REST
//! paths always did.

use async_graphql::{Request, Variables};
use chrono::Utc;
use rago_core::models as m;
use rago_db::Db;
use rago_graphql::{build_schema, Principal};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn kinds(db: &Db, org: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT webhook_type FROM webhooks WHERE organization_id = $1 ORDER BY created_at")
        .bind(org)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn customer_and_wallet_mutations_emit_their_webhooks(pool: PgPool) {
    let db = Db::new(pool.clone());
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
    db.insert_webhook_endpoint(org.id, "https://hooks.example.com/rago")
        .await
        .unwrap();
    let schema = build_schema(Db::new(pool));
    let run = |doc: &'static str, vars: serde_json::Value| {
        let req = Request::new(doc).variables(Variables::from_json(vars)).data(Principal {
            organization: org.clone(),
        });
        let schema = schema.clone();
        async move {
            let resp = schema.execute(req).await;
            assert!(resp.errors.is_empty(), "{:?}", resp.errors);
            serde_json::to_value(&resp.data).unwrap()
        }
    };

    let d = run(
        r#"mutation { createCustomer(input: { externalId: "c-1", name: "Ada", currency: "USD" }) { id } }"#,
        json!({}),
    )
    .await;
    let id = d["createCustomer"]["id"].as_str().unwrap().to_string();
    run(
        r#"mutation($id: ID!) { updateCustomer(input: { id: $id, name: "Ada L." }) { id } }"#,
        json!({ "id": id }),
    )
    .await;
    run(
        r#"mutation { createCustomerWallet(input: { externalCustomerId: "c-1", name: "Credits" }) { id } }"#,
        json!({}),
    )
    .await;

    assert_eq!(
        kinds(&db, org.id).await,
        vec!["customer.created", "customer.updated", "wallet.created"]
    );
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM webhooks WHERE organization_id = $1 AND webhook_type = 'customer.updated'",
    )
    .bind(org.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["customer"]["name"], "Ada L.", "{payload}");
}

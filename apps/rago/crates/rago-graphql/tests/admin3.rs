// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Live-DB schema-execution tests for the slice-3 admin surface: payment
//! providers, provider-customer connections, payment methods, manual payments +
//! payment requests, and integrations orchestration (config + mappings +
//! customers + fetch/sync). Runs on an isolated migrated Postgres via
//! `#[sqlx::test(migrator = "rago_db::MIGRATOR")]` (DB-less CI skips these).

use async_graphql::{Request, Variables};
use chrono::Utc;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use rago_graphql::{build_schema, Principal, RagoSchema};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn org() -> m::Organization {
    let now = Utc::now();
    m::Organization {
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
    }
}

fn customer(org: Uuid, external_id: &str) -> m::Customer {
    let now = Utc::now();
    m::Customer {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        external_id: external_id.into(),
        name: Some("C".into()),
        email: None,
        country: None,
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn invoice(org: Uuid, customer_id: Uuid) -> m::Invoice {
    let now = Utc::now();
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: None,
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 5000,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

async fn setup(pool: PgPool) -> (RagoSchema, Db, m::Organization) {
    // Provider/integration credentials are sealed at rest; storing one without
    // a key is refused (`encryption_key_missing`). Every test in this binary
    // uses the same fixed test key, so the process-wide env var is race-free.
    std::env::set_var(rago_db::secrets::ENCRYPTION_KEY_ENV, "admin3-test-key");
    let db = Db::new(pool.clone());
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    let schema = build_schema(Db::new(pool));
    (schema, db, organization)
}

async fn run(schema: &RagoSchema, org: &m::Organization, doc: &str, vars: serde_json::Value) -> async_graphql::Response {
    let req = Request::new(doc).variables(Variables::from_json(vars)).data(Principal { organization: org.clone() });
    schema.execute(req).await
}

fn data(resp: &async_graphql::Response) -> serde_json::Value {
    assert!(resp.errors.is_empty(), "graphql errors: {:?}", resp.errors);
    serde_json::to_value(&resp.data).unwrap()
}

// ---- payment providers -----------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn payment_provider_lifecycle(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    let resp = run(
        &schema,
        &org,
        r#"mutation { addStripePaymentProvider(input: { code: "stripe_1", name: "Stripe", secretKey: "sk_live_secret4242", supports3ds: true }) { id providerType code secretRef } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["addStripePaymentProvider"]["providerType"], "stripe");
    assert_eq!(d["addStripePaymentProvider"]["secretRef"], "****4242"); // masked, never raw
    let pp = d["addStripePaymentProvider"]["id"].as_str().unwrap().to_string();

    // update (create_or_update by code) — same code, new name
    let resp = run(&schema, &org, r#"mutation { updateStripePaymentProvider(input: { code: "stripe_1", name: "Stripe Prod" }) { name } }"#, json!({})).await;
    assert_eq!(data(&resp)["updateStripePaymentProvider"]["name"], "Stripe Prod");
    assert_eq!(db.list_payment_providers(org.id).await.unwrap().len(), 1); // upserted, not duplicated

    // add adyen (distinct)
    let resp = run(&schema, &org, r#"mutation { addAdyenPaymentProvider(input: { code: "adyen_1", name: "Adyen", apiKey: "AQE1234", merchantAccount: "Acme" }) { providerType } }"#, json!({})).await;
    assert_eq!(data(&resp)["addAdyenPaymentProvider"]["providerType"], "adyen");

    // query
    let resp = run(&schema, &org, r#"{ paymentProviders { code providerType } }"#, json!({})).await;
    assert_eq!(data(&resp)["paymentProviders"].as_array().unwrap().len(), 2);

    // destroy
    run(&schema, &org, r#"mutation($id: ID!) { destroyPaymentProvider(id: $id) }"#, json!({ "id": pp })).await;
    assert_eq!(db.list_payment_providers(org.id).await.unwrap().len(), 1);
}

// ---- payment provider customers + payment methods -------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn provider_customers_and_methods(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($c: ID!) { createPaymentProviderCustomer(input: { customerId: $c, paymentProvider: "stripe", providerCustomerId: "cus_123" }) { id providerType providerCustomerId isDefault } }"#,
        json!({ "c": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createPaymentProviderCustomer"]["providerCustomerId"], "cus_123");
    let ppc = d["createPaymentProviderCustomer"]["id"].as_str().unwrap().to_string();

    let resp = run(&schema, &org, r#"mutation($id: ID!) { setPaymentProviderCustomerAsDefault(id: $id) { isDefault } }"#, json!({ "id": ppc })).await;
    assert_eq!(data(&resp)["setPaymentProviderCustomerAsDefault"]["isDefault"], true);

    let resp = run(&schema, &org, r#"query($c: ID!) { paymentProviderCustomers(customerId: $c) { providerCustomerId } }"#, json!({ "c": c.id.to_string() })).await;
    assert_eq!(data(&resp)["paymentProviderCustomers"].as_array().unwrap().len(), 1);
    run(&schema, &org, r#"mutation($id: ID!) { destroyPaymentProviderCustomer(id: $id) }"#, json!({ "id": ppc })).await;
    assert_eq!(db.list_payment_provider_customers(c.id).await.unwrap().len(), 0);

    // payment method: seed then set-default + destroy
    let pm = Uuid::new_v4();
    db.insert_payment_method(pm, org.id, c.id, Some("pm_1"), false).await.unwrap();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { setPaymentMethodAsDefault(id: $id) { isDefault } }"#, json!({ "id": pm.to_string() })).await;
    assert_eq!(data(&resp)["setPaymentMethodAsDefault"]["isDefault"], true);
    let resp = run(&schema, &org, r#"query($c: ID!) { paymentMethods(customerId: $c) { providerMethodId } }"#, json!({ "c": c.id.to_string() })).await;
    assert_eq!(data(&resp)["paymentMethods"][0]["providerMethodId"], "pm_1");
    run(&schema, &org, r#"mutation($id: ID!) { destroyPaymentMethod(id: $id) }"#, json!({ "id": pm.to_string() })).await;
    assert_eq!(db.list_payment_methods(c.id).await.unwrap().len(), 0);
}

// ---- manual payment + payment request -------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn manual_payment_and_request(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-p");
    db.insert_customer(&c).await.unwrap();
    let inv = invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($i: ID!) { createPayment(input: { invoiceId: $i, amountCents: 5000, reference: "wire-001" }) { provider status amountCents } }"#,
        json!({ "i": inv.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createPayment"]["provider"], "manual");
    assert_eq!(d["createPayment"]["amountCents"], 5000);
    assert_eq!(db.list_payments(inv.id).await.unwrap().len(), 1);

    let resp = run(
        &schema,
        &org,
        r#"mutation { createPaymentRequest(input: { externalCustomerId: "cust-p", email: "p@acme.test", amountCents: 5000, amountCurrency: "USD" }) { id externalCustomerId paymentStatus } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createPaymentRequest"]["paymentStatus"], "pending");
    let resp = run(&schema, &org, r#"{ paymentRequests(externalCustomerId: "cust-p") { email } }"#, json!({})).await;
    assert_eq!(data(&resp)["paymentRequests"][0]["email"], "p@acme.test");

    // generate payment url: with no payment provider configured for the org
    // the mutation refuses with an explicit code rather than fabricating a URL.
    let resp = run(&schema, &org, r#"mutation($i: ID!) { generatePaymentUrl(invoiceId: $i) { paymentUrl } }"#, json!({ "i": inv.id.to_string() })).await;
    assert_eq!(resp.errors.len(), 1, "{:?}", resp.errors);
    assert_eq!(resp.errors[0].message, "payment_provider_not_configured");
}

// ---- integrations config + mappings + customers ---------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn integration_config_and_mappings(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-i");
    db.insert_customer(&c).await.unwrap();

    // create netsuite integration (secrets masked into settings)
    let resp = run(
        &schema,
        &org,
        r#"mutation { createNetsuiteIntegration(input: { code: "ns1", name: "NetSuite", accountId: "acc", clientId: "cid", clientSecret: "supersecret9999", connectionId: "conn", scriptEndpointUrl: "https://x", tokenId: "tid", tokenSecret: "tok", syncInvoices: true }) { id providerCode code name } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createNetsuiteIntegration"]["providerCode"], "netsuite");
    assert_eq!(d["createNetsuiteIntegration"]["code"], "ns1");
    let int_id = d["createNetsuiteIntegration"]["id"].as_str().unwrap().to_string();
    // secret masked in settings, never raw
    let settings = db.find_integration(Uuid::parse_str(&int_id).unwrap()).await.unwrap().unwrap().settings;
    assert_eq!(settings["client_secret_ref"], "****9999");

    // update + list + show
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateIntegration(input: { id: $id, name: "NetSuite Prod" }) { name } }"#, json!({ "id": int_id })).await;
    assert_eq!(data(&resp)["updateIntegration"]["name"], "NetSuite Prod");
    let resp = run(&schema, &org, r#"{ integrations { providerCode code } }"#, json!({})).await;
    assert_eq!(data(&resp)["integrations"].as_array().unwrap().len(), 1);
    let resp = run(&schema, &org, r#"query($id: ID!) { integration(id: $id) { code } }"#, json!({ "id": int_id })).await;
    assert_eq!(data(&resp)["integration"]["code"], "ns1");

    // collection mapping
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { createIntegrationCollectionMapping(input: { integrationId: $id, mappingType: "fallback_item", externalId: "ext-1", externalName: "Fallback" }) { id mappingType externalName } }"#,
        json!({ "id": int_id }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createIntegrationCollectionMapping"]["mappingType"], "fallback_item");
    let icm = d["createIntegrationCollectionMapping"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateIntegrationCollectionMapping(input: { id: $id, externalName: "Fallback2" }) { externalName } }"#, json!({ "id": icm })).await;
    assert_eq!(data(&resp)["updateIntegrationCollectionMapping"]["externalName"], "Fallback2");
    let resp = run(&schema, &org, r#"query($id: ID!) { integrationCollectionMappings(integrationId: $id) { id } }"#, json!({ "id": int_id })).await;
    assert_eq!(data(&resp)["integrationCollectionMappings"].as_array().unwrap().len(), 1);
    run(&schema, &org, r#"mutation($id: ID!) { destroyIntegrationCollectionMapping(id: $id) }"#, json!({ "id": icm })).await;
    assert_eq!(db.list_integration_collection_mappings(Uuid::parse_str(&int_id).unwrap()).await.unwrap().len(), 0);

    // resource mapping
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!, $m: ID!) { createIntegrationMapping(input: { integrationId: $id, mappableType: "AddOn", mappableId: $m, externalId: "ext-addon" }) { id mappableType externalId } }"#,
        json!({ "id": int_id, "m": Uuid::new_v4().to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createIntegrationMapping"]["mappableType"], "AddOn");
    let im = d["createIntegrationMapping"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"query($id: ID!) { integrationMappings(integrationId: $id) { externalId } }"#, json!({ "id": int_id })).await;
    assert_eq!(data(&resp)["integrationMappings"][0]["externalId"], "ext-addon");
    run(&schema, &org, r#"mutation($id: ID!) { destroyIntegrationMapping(id: $id) }"#, json!({ "id": im })).await;

    // integration customer + set default
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!, $c: ID!) { createIntegrationCustomer(input: { integrationId: $id, customerId: $c, externalCustomerId: "ext-cust" }) { id externalCustomerId } }"#,
        json!({ "id": int_id, "c": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    let ic = d["createIntegrationCustomer"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { setIntegrationCustomerAsDefault(id: $id) { isDefault } }"#, json!({ "id": ic })).await;
    assert_eq!(data(&resp)["setIntegrationCustomerAsDefault"]["isDefault"], true);
    let resp = run(&schema, &org, r#"query($c: ID!) { integrationCustomers(customerId: $c) { externalCustomerId } }"#, json!({ "c": c.id.to_string() })).await;
    assert_eq!(data(&resp)["integrationCustomers"][0]["externalCustomerId"], "ext-cust");

    // fetch items: a real NetSuite catalog call. This test build has outbound
    // HTTP disabled (no `live` feature), so it must surface that as an error
    // rather than fabricate an empty catalog.
    let resp = run(&schema, &org, r#"mutation($id: ID!) { fetchIntegrationItems(integrationId: $id) { id } }"#, json!({ "id": int_id })).await;
    assert_eq!(resp.errors.len(), 1, "{:?}", resp.errors);
    assert!(resp.errors[0].message.starts_with("integration error"), "{}", resp.errors[0].message);

    // destroy integration
    run(&schema, &org, r#"mutation($id: ID!) { destroyIntegration(id: $id) }"#, json!({ "id": int_id })).await;
    assert!(db.find_integration(Uuid::parse_str(&int_id).unwrap()).await.unwrap().is_none());
}

// ---- integration sync enqueues a durable job ------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn sync_invoice_enqueues_job(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-s");
    db.insert_customer(&c).await.unwrap();
    let inv = invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();

    let resp = run(&schema, &org, r#"mutation($i: ID!) { syncIntegrationInvoice(invoiceId: $i) }"#, json!({ "i": inv.id.to_string() })).await;
    data(&resp);
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type = 'integrations.sync_invoice'").fetch_one(&db.pool).await.unwrap();
    assert_eq!(jobs, 1);
}

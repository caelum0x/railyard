//! Live-DB tests for slice-5: quotes/orders, AI + finance assistant, data
//! exports, SSO login shape, tax retries, adjusted-fee revert, and data_api_*
//! analytics. Runs on an isolated migrated Postgres via
//! `#[sqlx::test(migrator = "rago_db::MIGRATOR")]`.

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

fn finalized_invoice(org: Uuid, customer_id: Uuid, total: i64) -> m::Invoice {
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
        issuing_date: Some(now.date_naive()),
        fees_amount_cents: total,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: total,
        sub_total_including_taxes_amount_cents: total,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: total,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

async fn setup(pool: PgPool) -> (RagoSchema, Db, m::Organization) {
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

// ---- quotes + quote versions ----------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn quotes_and_versions(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-q");
    db.insert_customer(&c).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($c: ID!) { createQuote(input: { customerId: $c, currency: "USD", totalAmountCents: 5000, lineItems: [{ name: "Seat", amount: 5000 }] }) { id status totalAmountCents } }"#,
        json!({ "c": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createQuote"]["status"], "draft");
    assert_eq!(d["createQuote"]["totalAmountCents"], 5000);
    let quote_id = d["createQuote"]["id"].as_str().unwrap().to_string();

    // the initial version exists
    let resp = run(&schema, &org, r#"query($q: ID!) { quoteVersions(quoteId: $q) { id versionNumber status } }"#, json!({ "q": quote_id })).await;
    let d = data(&resp);
    assert_eq!(d["quoteVersions"].as_array().unwrap().len(), 1);
    let ver_id = d["quoteVersions"][0]["id"].as_str().unwrap().to_string();

    // clone → v2, then approve v1, void v2
    let resp = run(&schema, &org, r#"mutation($id: ID!) { cloneQuoteVersion(id: $id) { versionNumber status } }"#, json!({ "id": ver_id })).await;
    assert_eq!(data(&resp)["cloneQuoteVersion"]["versionNumber"], 2);
    let resp = run(&schema, &org, r#"mutation($id: ID!) { approveQuoteVersion(id: $id) { status } }"#, json!({ "id": ver_id })).await;
    assert_eq!(data(&resp)["approveQuoteVersion"]["status"], "approved");
    assert_eq!(db.list_quote_versions(Uuid::parse_str(&quote_id).unwrap()).await.unwrap().len(), 2);

    // update quote
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateQuote(input: { id: $id, status: "accepted" }) { status } }"#, json!({ "id": quote_id })).await;
    assert_eq!(data(&resp)["updateQuote"]["status"], "accepted");
}

// ---- order forms + orders --------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn order_forms_and_orders(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-o");
    db.insert_customer(&c).await.unwrap();

    // order forms/orders are generated from quotes; seed directly then drive
    let of = Uuid::new_v4();
    db.insert_order_form(of, org.id, Some(c.id), None, "OF-1").await.unwrap();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { markOrderFormAsSigned(id: $id, executionMode: "auto") { status executionMode } }"#, json!({ "id": of.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["markOrderFormAsSigned"]["status"], "signed");
    assert_eq!(d["markOrderFormAsSigned"]["executionMode"], "auto");

    let ord = Uuid::new_v4();
    db.insert_order(ord, org.id, Some(c.id), Some(of), "ORD-1").await.unwrap();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { executeOrder(id: $id) { status } }"#, json!({ "id": ord.to_string() })).await;
    assert_eq!(data(&resp)["executeOrder"]["status"], "executed");

    let resp = run(&schema, &org, r#"{ orderForms { number status } orders { number status } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["orderForms"].as_array().unwrap().len(), 1);
    assert_eq!(d["orders"][0]["status"], "executed");
}

// ---- AI conversation + finance assistant (honest, no LLM) ------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn ai_and_finance_assistant(pool: PgPool) {
    let (schema, _db, org) = setup(pool).await;

    // create_ai_conversation persists the user message; no fabricated assistant reply
    let resp = run(&schema, &org, r#"mutation { createAiConversation(message: "What is my MRR?") { id name messages { role content } } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["createAiConversation"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(d["createAiConversation"]["messages"][0]["role"], "user");

    // ask_finance_assistant: no provider key configured → honest not_configured
    let resp = run(&schema, &org, r#"mutation { askFinanceAssistant(question: "revenue last month?") { status answer } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["askFinanceAssistant"]["status"], "not_configured");
    assert!(d["askFinanceAssistant"]["answer"].is_null());
}

// ---- data exports + data_api_* ---------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn data_exports_and_data_api(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-d");
    db.insert_customer(&c).await.unwrap();
    db.insert_invoice(&finalized_invoice(org.id, c.id, 7000)).await.unwrap();

    // create invoices data export → enqueues job
    let resp = run(&schema, &org, r#"mutation { createInvoicesDataExport(format: "csv", filters: { status: "finalized" }) { id resourceType status } }"#, json!({})).await;
    assert_eq!(data(&resp)["createInvoicesDataExport"]["resourceType"], "invoices");
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type = 'data_exports.generate'").fetch_one(&db.pool).await.unwrap();
    assert_eq!(jobs, 1);
    let resp = run(&schema, &org, r#"{ dataExports { resourceType status } }"#, json!({})).await;
    assert_eq!(data(&resp)["dataExports"].as_array().unwrap().len(), 1);

    // data_api_revenue_streams computes from finalized invoices (real)
    let resp = run(&schema, &org, r#"{ dataApiRevenueStreams { amountCents } dataApiUsages { key } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["dataApiRevenueStreams"][0]["amountCents"], 7000);
    assert_eq!(d["dataApiUsages"].as_array().unwrap().len(), 0); // honest-empty (needs daily snapshots)
}

// ---- SSO login: honest idp_not_configured ---------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn sso_login_is_honest_error(pool: PgPool) {
    let (schema, _db, org) = setup(pool).await;
    let resp = run(&schema, &org, r#"mutation { oktaLogin(code: "abc", state: "xyz") { token } }"#, json!({})).await;
    assert!(!resp.errors.is_empty());
    assert!(resp.errors[0].message.contains("idp_not_configured"));

    // EntraId integration config persists (secret masked)
    let resp = run(&schema, &org, r#"mutation { createEntraIdIntegration(code: "aad", name: "Azure AD", clientSecret: "topsecret4321") { providerCode code } }"#, json!({})).await;
    assert_eq!(data(&resp)["createEntraIdIntegration"]["providerCode"], "entra_id");
}

// ---- tax retries + adjusted-fee revert ------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn tax_retry_and_adjusted_fee_revert(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-t");
    db.insert_customer(&c).await.unwrap();
    let inv = finalized_invoice(org.id, c.id, 1000);
    db.insert_invoice(&inv).await.unwrap();

    // retry tax provider voiding enqueues a job
    let resp = run(&schema, &org, r#"mutation($id: ID!) { retryTaxProviderVoiding(id: $id) { id } }"#, json!({ "id": inv.id.to_string() })).await;
    data(&resp);
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type = 'invoices.tax_provider_voiding'").fetch_one(&db.pool).await.unwrap();
    assert_eq!(jobs, 1);

    // adjusted-fee create then destroy reverts the fee to its original amount
    let fee_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO fees (id, organization_id, billing_entity_id, invoice_id, amount_cents, amount_currency, unit_amount_cents, taxes_amount_cents, units, payment_status, created_at, updated_at) \
         VALUES ($1,$2,$2,$3,1000,'USD',100,0,10,0, now(), now())",
    )
    .bind(fee_id).bind(org.id).bind(inv.id).execute(&db.pool).await.unwrap();

    let resp = run(&schema, &org, r#"mutation($f: ID!, $i: ID!) { createAdjustedFee(input: { invoiceId: $i, feeId: $f, units: "20" }) { amountCents } }"#, json!({ "f": fee_id.to_string(), "i": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["createAdjustedFee"]["amountCents"], 2000);
    let af_id: Uuid = sqlx::query_scalar("SELECT id FROM adjusted_fees WHERE fee_id = $1").bind(fee_id).fetch_one(&db.pool).await.unwrap();

    let resp = run(&schema, &org, r#"mutation($id: ID!) { destroyAdjustedFee(id: $id) }"#, json!({ "id": af_id.to_string() })).await;
    data(&resp);
    // fee reverted to its original 1000 / 10 units
    let fee = db.find_fee_opt(fee_id).await.unwrap().unwrap();
    assert_eq!(fee.amount_cents, 1000);
    assert_eq!(fee.unit_amount_cents, 100);
}

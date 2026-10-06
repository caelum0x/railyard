// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Live-DB schema-execution tests for the slice-2 admin surface: one-off/batch
//! invoices, usage-monitoring alerts, billing entities, invoice custom sections,
//! adjusted fees, fixed charges, subscription entitlements, and credit-note item
//! persistence. Each runs on an isolated migrated Postgres via
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

fn plan(org: Uuid, code: &str) -> m::Plan {
    let now = Utc::now();
    m::Plan {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "Starter".into(),
        code: code.into(),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(1000),
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

fn draft_invoice(org: Uuid, customer_id: Uuid, status: InvoiceStatus) -> m::Invoice {
    let now = Utc::now();
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status,
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
        taxes_rate: 20.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

async fn insert_fee(db: &Db, org: Uuid, invoice_id: Uuid, amount_cents: i64, units: i64, unit_amount_cents: i64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO fees (id, organization_id, billing_entity_id, invoice_id, amount_cents, amount_currency, unit_amount_cents, taxes_amount_cents, units, payment_status, created_at, updated_at) \
         VALUES ($1,$2,$2,$3,$4,'USD',$5,0,$6,0, now(), now())",
    )
    .bind(id)
    .bind(org)
    .bind(invoice_id)
    .bind(amount_cents)
    .bind(unit_amount_cents)
    .bind(rust_decimal::Decimal::from(units))
    .execute(&db.pool)
    .await
    .unwrap();
    id
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

// ---- one-off + batch invoices ---------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_one_off_and_batch_invoices(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($c: ID!) {
             createInvoice(input: { customerId: $c, currency: "USD",
               fees: [{ unitAmountCents: 500, units: "3" }] }) { id invoiceType feesAmountCents totalAmountCents }
           }"#,
        json!({ "c": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createInvoice"]["invoiceType"], "one_off");
    assert_eq!(d["createInvoice"]["feesAmountCents"], 1500);
    assert_eq!(d["createInvoice"]["totalAmountCents"], 1500);

    // finalize all: seed a draft
    let inv = draft_invoice(org.id, c.id, InvoiceStatus::Draft);
    db.insert_invoice(&inv).await.unwrap();
    let resp = run(&schema, &org, r#"mutation { finalizeAllInvoices { collection { status } metadata { totalCount } } }"#, json!({})).await;
    let d = data(&resp);
    assert!(d["finalizeAllInvoices"]["metadata"]["totalCount"].as_i64().unwrap() >= 1);

    // retry all payments: seed a failed-payment finalized invoice
    let mut failed = draft_invoice(org.id, c.id, InvoiceStatus::Finalized);
    failed.payment_status = PaymentStatus::Failed;
    db.insert_invoice(&failed).await.unwrap();
    let resp = run(&schema, &org, r#"mutation { retryAllInvoicePayments { collection { id } } }"#, json!({})).await;
    data(&resp);
    assert_eq!(db.find_invoice(failed.id).await.unwrap().unwrap().payment_status, PaymentStatus::Pending);

    // regenerate: only a VOIDED invoice can be re-issued (finalizeAll above
    // finalized `inv`, so it is refused until it is voided).
    let regen = r#"mutation($id: ID!) { regenerateInvoice(voidedInvoiceId: $id) { id status } }"#;
    let resp = run(&schema, &org, regen, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(resp.errors[0].message, "invoice_not_voided");
    // the re-issued invoice copies the voided one's fees
    insert_fee(&db, org.id, inv.id, 1500, 3, 500).await;
    let resp = run(&schema, &org, r#"mutation($id: ID!) { voidInvoice(id: $id) { status } }"#, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["voidInvoice"]["status"], "voided");
    let resp = run(&schema, &org, regen, json!({ "id": inv.id.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["regenerateInvoice"]["status"], "finalized");
    let new_id = Uuid::parse_str(d["regenerateInvoice"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(db.fees_for_invoice(new_id).await.unwrap().len(), 1);
    assert_ne!(d["regenerateInvoice"]["id"], inv.id.to_string());
}

// ---- alerts (subscription + wallet) ---------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn subscription_and_wallet_alerts(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let sub_id = Uuid::new_v4();
    let wallet_id = Uuid::new_v4();

    // create subscription alert
    let resp = run(
        &schema,
        &org,
        r#"mutation($s: ID!) {
             createSubscriptionAlert(input: { subscriptionId: $s, alertType: "current_usage_amount", code: "usage",
               thresholds: [{ value: "1000", recurring: false }, { value: "5000" }] }) { id code alertableType thresholds { value } }
           }"#,
        json!({ "s": sub_id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createSubscriptionAlert"]["alertableType"], "Subscription");
    assert_eq!(d["createSubscriptionAlert"]["thresholds"].as_array().unwrap().len(), 2);
    let alert_id = d["createSubscriptionAlert"]["id"].as_str().unwrap().to_string();
    assert_eq!(db.alerts_for("Subscription", sub_id).await.unwrap().len(), 1);

    // update alert (replace thresholds)
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateSubscriptionAlert(input: { id: $id, name: "High usage", thresholds: [{ value: "9000" }] }) { name thresholds { value } } }"#,
        json!({ "id": alert_id }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateSubscriptionAlert"]["name"], "High usage");
    assert_eq!(d["updateSubscriptionAlert"]["thresholds"].as_array().unwrap().len(), 1);
    assert_eq!(db.alert_thresholds(Uuid::parse_str(&alert_id).unwrap()).await.unwrap().len(), 1);

    // wallet alert create + destroy
    let resp = run(
        &schema,
        &org,
        r#"mutation($w: ID!) { createCustomerWalletAlert(input: { walletId: $w, alertType: "current_usage_amount", code: "wusage", thresholds: [{ value: "100" }] }) { id alertableType } }"#,
        json!({ "w": wallet_id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createCustomerWalletAlert"]["alertableType"], "Wallet");
    let wid = d["createCustomerWalletAlert"]["id"].as_str().unwrap().to_string();
    run(&schema, &org, r#"mutation($id: ID!) { destroyCustomerWalletAlert(id: $id) }"#, json!({ "id": wid })).await;
    assert_eq!(db.alerts_for("Wallet", wallet_id).await.unwrap().len(), 0);

    // query
    let resp = run(&schema, &org, r#"query($s: ID!) { subscriptionAlerts(subscriptionId: $s) { code } }"#, json!({ "s": sub_id.to_string() })).await;
    assert_eq!(data(&resp)["subscriptionAlerts"].as_array().unwrap().len(), 1);
}

// ---- billing entities + invoice custom sections ---------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn billing_entities_and_custom_sections(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    let resp = run(
        &schema,
        &org,
        r#"mutation { createBillingEntity(input: { code: "eu", name: "EU Entity", defaultCurrency: "EUR" }) { id code defaultCurrency } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createBillingEntity"]["defaultCurrency"], "EUR");
    let be = d["createBillingEntity"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateBillingEntity(input: { id: $id, name: "EU GmbH" }) { name } }"#, json!({ "id": be })).await;
    assert_eq!(data(&resp)["updateBillingEntity"]["name"], "EU GmbH");
    run(&schema, &org, r#"mutation($id: ID!) { destroyBillingEntity(id: $id) }"#, json!({ "id": be })).await;
    assert!(db.find_billing_entity(Uuid::parse_str(&be).unwrap()).await.unwrap().is_none());

    let resp = run(
        &schema,
        &org,
        r#"mutation { createInvoiceCustomSection(input: { code: "footer", name: "Footer", details: "Thanks" }) { id name details } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createInvoiceCustomSection"]["details"], "Thanks");
    let ics = d["createInvoiceCustomSection"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateInvoiceCustomSection(input: { id: $id, name: "Footer2" }) { name } }"#, json!({ "id": ics })).await;
    assert_eq!(data(&resp)["updateInvoiceCustomSection"]["name"], "Footer2");
    run(&schema, &org, r#"mutation($id: ID!) { destroyInvoiceCustomSection(id: $id) }"#, json!({ "id": ics })).await;
    assert_eq!(db.list_invoice_custom_sections(org.id).await.unwrap().len(), 0);
}

// ---- adjusted fees ---------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn adjusted_fee_create_preview_destroy(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-af");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id, InvoiceStatus::Draft);
    db.insert_invoice(&inv).await.unwrap();
    let fee_id = insert_fee(&db, org.id, inv.id, 1000, 10, 100).await;

    // preview: does not persist
    let resp = run(
        &schema,
        &org,
        r#"mutation($f: ID!, $i: ID!) { previewAdjustedFee(input: { invoiceId: $i, feeId: $f, units: "20" }) { amountCents units } }"#,
        json!({ "f": fee_id.to_string(), "i": inv.id.to_string() }),
    )
    .await;
    assert_eq!(data(&resp)["previewAdjustedFee"]["amountCents"], 2000);
    assert_eq!(db.find_fee_opt(fee_id).await.unwrap().unwrap().amount_cents, 1000); // unchanged

    // create: persists override + adjusted_fees row
    let resp = run(
        &schema,
        &org,
        r#"mutation($f: ID!, $i: ID!) { createAdjustedFee(input: { invoiceId: $i, feeId: $f, units: "20" }) { id amountCents } }"#,
        json!({ "f": fee_id.to_string(), "i": inv.id.to_string() }),
    )
    .await;
    assert_eq!(data(&resp)["createAdjustedFee"]["amountCents"], 2000);
    assert_eq!(db.find_fee_opt(fee_id).await.unwrap().unwrap().amount_cents, 2000);
    let af_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM adjusted_fees WHERE fee_id = $1").bind(fee_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(af_count, 1);
}

// ---- fixed charges ---------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn fixed_charge_crud(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let p = plan(org.id, "starter");
    db.insert_plan(&p).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($p: ID!) { createFixedCharge(input: { planId: $p, chargeModel: "standard", units: "2", properties: { amount: "50" } }) { id chargeModel units } }"#,
        json!({ "p": p.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createFixedCharge"]["chargeModel"], "standard");
    let fc = d["createFixedCharge"]["id"].as_str().unwrap().to_string();
    assert_eq!(db.fixed_charges_for_plan(p.id).await.unwrap().len(), 1);

    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateFixedCharge(input: { id: $id, payInAdvance: true, units: "5" }) { payInAdvance units } }"#, json!({ "id": fc })).await;
    let d = data(&resp);
    assert_eq!(d["updateFixedCharge"]["payInAdvance"], true);
    assert_eq!(d["updateFixedCharge"]["units"], "5");

    // query
    let resp = run(&schema, &org, r#"query($p: ID!) { fixedCharges(planId: $p) { id } }"#, json!({ "p": p.id.to_string() })).await;
    assert_eq!(data(&resp)["fixedCharges"].as_array().unwrap().len(), 1);

    run(&schema, &org, r#"mutation($id: ID!) { destroyFixedCharge(id: $id) }"#, json!({ "id": fc })).await;
    assert_eq!(db.fixed_charges_for_plan(p.id).await.unwrap().len(), 0);
}

// ---- subscription entitlements --------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn subscription_entitlement_attach_detach(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let feature_id = Uuid::new_v4();
    db.insert_feature(feature_id, org.id, "seats", Some("Seats"), None).await.unwrap();
    let sub_id = Uuid::new_v4();

    let resp = run(&schema, &org, r#"mutation($s: ID!) { createSubscriptionEntitlement(subscriptionId: $s, featureCode: "seats") }"#, json!({ "s": sub_id.to_string() })).await;
    data(&resp);
    let resp = run(&schema, &org, r#"query($s: ID!) { subscriptionEntitlements(subscriptionId: $s) { featureCode } }"#, json!({ "s": sub_id.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["subscriptionEntitlements"][0]["featureCode"], "seats");

    // features query
    let resp = run(&schema, &org, r#"{ features { code } }"#, json!({})).await;
    assert_eq!(data(&resp)["features"][0]["code"], "seats");

    run(&schema, &org, r#"mutation($s: ID!) { destroySubscriptionEntitlement(subscriptionId: $s, featureCode: "seats") }"#, json!({ "s": sub_id.to_string() })).await;
    let resp = run(&schema, &org, r#"query($s: ID!) { subscriptionEntitlements(subscriptionId: $s) { featureCode } }"#, json!({ "s": sub_id.to_string() })).await;
    assert_eq!(data(&resp)["subscriptionEntitlements"].as_array().unwrap().len(), 0);
}

// ---- credit-note item persistence -----------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn credit_note_items_persisted_with_taxes(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-cn");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id, InvoiceStatus::Finalized); // taxes_rate = 20.0
    db.insert_invoice(&inv).await.unwrap();
    let fee_id = insert_fee(&db, org.id, inv.id, 1000, 1, 1000).await;

    let resp = run(
        &schema,
        &org,
        r#"mutation($inv: ID!, $fee: ID!) {
             createCreditNote(input: { invoiceId: $inv, reason: "other",
               items: [{ feeId: $fee, amountCents: 1000 }] }) { id creditAmountCents taxesAmountCents }
           }"#,
        json!({ "inv": inv.id.to_string(), "fee": fee_id.to_string() }),
    )
    .await;
    let d = data(&resp);
    // credit derived from items sum (1000); taxes = 20% of total
    assert_eq!(d["createCreditNote"]["creditAmountCents"], 1000);
    assert_eq!(d["createCreditNote"]["taxesAmountCents"], 200);
    let cn_id = Uuid::parse_str(d["createCreditNote"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(db.credit_note_items(cn_id).await.unwrap().len(), 1);
    assert_eq!(db.credit_note_items(cn_id).await.unwrap()[0], (fee_id, 1000));
}

// ---- void is org-scoped ----------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn void_invoice_rejects_cross_org_id(pool: PgPool) {
    let (schema, db, org_a) = setup(pool).await;
    let c = customer(org_a.id, "cust-void");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org_a.id, c.id, InvoiceStatus::Finalized);
    db.insert_invoice(&inv).await.unwrap();
    let org_b = org();
    db.insert_organization(&org_b).await.unwrap();

    let void = r#"mutation($id: ID!) { voidInvoice(id: $id) { status } }"#;
    let resp = run(&schema, &org_b, void, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(resp.errors[0].message, "invoice_not_found");
    assert_eq!(db.find_invoice(inv.id).await.unwrap().unwrap().status, InvoiceStatus::Finalized);

    let resp = run(&schema, &org_a, void, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["voidInvoice"]["status"], "voided");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn regenerate_invoice_with_adjusted_fees_recomputes_totals(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-r");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id, InvoiceStatus::Finalized);
    db.insert_invoice(&inv).await.unwrap();
    let keep = insert_fee(&db, org.id, inv.id, 1500, 3, 500).await;
    let drop = insert_fee(&db, org.id, inv.id, 700, 1, 700).await;
    let resp = run(&schema, &org, r#"mutation($id: ID!) { voidInvoice(id: $id) { status } }"#, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["voidInvoice"]["status"], "voided");
    let regen = r#"mutation($id: ID!, $fees: [RegenerateFeeInput!]) {
        regenerateInvoice(voidedInvoiceId: $id, fees: $fees) { id feesAmountCents totalAmountCents } }"#;
    // An unknown fee id is refused.
    let resp = run(&schema, &org, regen, json!({ "id": inv.id.to_string(), "fees": [{ "id": Uuid::new_v4().to_string(), "units": "1" }] })).await;
    assert_eq!(resp.errors[0].message, "fee_not_on_invoice");
    let resp = run(
        &schema,
        &org,
        regen,
        json!({ "id": inv.id.to_string(), "fees": [
            { "id": keep.to_string(), "units": "2", "unitAmountCents": 450 },
            { "id": drop.to_string(), "units": "0" },
        ] }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["regenerateInvoice"]["feesAmountCents"], 900, "{d}");
    assert_eq!(d["regenerateInvoice"]["totalAmountCents"], 900);
    let new_id = Uuid::parse_str(d["regenerateInvoice"]["id"].as_str().unwrap()).unwrap();
    let fees = db.fees_for_invoice(new_id).await.unwrap();
    assert_eq!(fees.len(), 1);
    assert_eq!((fees[0].amount_cents, fees[0].unit_amount_cents), (900, 450));
}

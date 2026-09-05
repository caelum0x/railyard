//! Live-DB schema-execution tests for the GraphQL dashboard admin CRUD surface.
//!
//! Each test runs on an isolated, migrated Postgres via
//! `#[sqlx::test(migrator = "rago_db::MIGRATOR")]` (DB-less CI simply skips these
//! — sqlx requires `DATABASE_URL`). Every case executes a real GraphQL mutation
//! through the schema with an authenticated [`Principal`], then asserts both the
//! returned shape and the persisted DB change.

use async_graphql::{Request, Variables};
use chrono::Utc;
use rago_core::enums::*;
use rago_core::models as m;
use rago_db::Db;
use rago_graphql::{build_schema, Principal, RagoSchema};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// ---- seed helpers ----------------------------------------------------------

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

fn customer(org: Uuid, external_id: &str) -> m::Customer {
    let now = Utc::now();
    m::Customer {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        external_id: external_id.into(),
        name: Some("Original".into()),
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

fn metric(org: Uuid, code: &str) -> m::BillableMetric {
    let now = Utc::now();
    m::BillableMetric {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "API Calls".into(),
        code: code.into(),
        description: None,
        aggregation_type: AggregationType::CountAgg,
        field_name: None,
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn coupon(org: Uuid, code: &str) -> m::Coupon {
    let now = Utc::now();
    m::Coupon {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "Launch".into(),
        code: code.into(),
        status: CouponStatus::Active,
        coupon_type: CouponType::FixedAmount,
        amount_cents: Some(500),
        amount_currency: Some("USD".into()),
        percentage_rate: None,
        frequency: CouponFrequency::Once,
        frequency_duration: None,
        expiration: CouponExpiration::NoExpiration,
        expiration_at: None,
        reusable: true,
        limited_plans: false,
        limited_billable_metrics: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn draft_invoice(org: Uuid, customer_id: Uuid) -> m::Invoice {
    let now = Utc::now();
    m::Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Draft,
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
        total_amount_cents: 0,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

/// Build a schema over the pool and seed an organization; returns the schema,
/// a `Db` handle for direct assertions, and the seeded org.
async fn setup(pool: PgPool) -> (RagoSchema, Db, m::Organization) {
    let db = Db::new(pool.clone());
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    let schema = build_schema(Db::new(pool));
    (schema, db, organization)
}

/// Execute a GraphQL document as the given organization (authenticated).
async fn run(
    schema: &RagoSchema,
    org: &m::Organization,
    doc: &str,
    vars: serde_json::Value,
) -> async_graphql::Response {
    let req = Request::new(doc)
        .variables(Variables::from_json(vars))
        .data(Principal { organization: org.clone() });
    schema.execute(req).await
}

fn data(resp: &async_graphql::Response) -> serde_json::Value {
    assert!(resp.errors.is_empty(), "graphql errors: {:?}", resp.errors);
    serde_json::to_value(&resp.data).unwrap()
}

// ---- customers -------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_and_destroy_customer(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-1");
    db.insert_customer(&c).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) {
             updateCustomer(input: { id: $id, name: "Renamed", email: "new@acme.test" }) {
               id name email
             }
           }"#,
        json!({ "id": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateCustomer"]["name"], "Renamed");
    let persisted = db.find_customer(c.id).await.unwrap().unwrap();
    assert_eq!(persisted.name.as_deref(), Some("Renamed"));
    assert_eq!(persisted.email.as_deref(), Some("new@acme.test"));

    // grace period mutation
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateCustomerInvoiceGracePeriod(id: $id, invoiceGracePeriod: 7) { id } }"#,
        json!({ "id": c.id.to_string() }),
    )
    .await;
    data(&resp);

    // destroy → soft delete
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { destroyCustomer(id: $id) }"#,
        json!({ "id": c.id.to_string() }),
    )
    .await;
    data(&resp);
    assert!(db.find_customer_by_external_id(org.id, "cust-1").await.unwrap().is_none());
}

// ---- taxes -----------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn tax_crud_lifecycle(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    let resp = run(
        &schema,
        &org,
        r#"mutation { createTax(input: { name: "VAT", code: "vat", rate: 20.0 }) { id name rate } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createTax"]["rate"], 20.0);
    let id = d["createTax"]["id"].as_str().unwrap().to_string();
    assert_eq!(db.list_taxes(org.id).await.unwrap().len(), 1);

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateTax(input: { id: $id, rate: 15.0, appliedToOrganization: true }) { rate appliedToOrganization } }"#,
        json!({ "id": id }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateTax"]["rate"], 15.0);
    let tax = db.find_tax(Uuid::parse_str(&id).unwrap()).await.unwrap().unwrap();
    assert_eq!(tax.rate, 15.0);
    assert!(tax.applied_to_organization);

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { destroyTax(id: $id) }"#,
        json!({ "id": id }),
    )
    .await;
    data(&resp);
    assert_eq!(db.list_taxes(org.id).await.unwrap().len(), 0);

    // list + show queries
    let resp = run(&schema, &org, r#"{ taxes { collection { id } metadata { totalCount } } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["taxes"]["metadata"]["totalCount"], 0);
}

// ---- plans -----------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_and_destroy_plan(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let p = plan(org.id, "starter");
    db.insert_plan(&p).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updatePlan(input: { id: $id, name: "Pro", amountCents: 4200 }) { name amountCents } }"#,
        json!({ "id": p.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updatePlan"]["name"], "Pro");
    assert_eq!(d["updatePlan"]["amountCents"], 4200);
    let updated = db.find_plan_opt(p.id).await.unwrap().unwrap();
    assert_eq!(updated.amount_cents, Some(4200));

    let resp = run(&schema, &org, r#"mutation($id: ID!) { destroyPlan(id: $id) }"#, json!({ "id": p.id.to_string() })).await;
    data(&resp);
    assert!(db.find_plan_by_code(org.id, "starter").await.unwrap().is_none());
}

// ---- webhook emit at mutation trigger points -------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn mutation_emits_webhook_when_endpoint_configured(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    // No endpoint configured yet → the createBillableMetric emit is a no-op.
    let resp = run(
        &schema,
        &org,
        r#"mutation { createBillableMetric(input: { name: "API", code: "api_noop", aggregationType: "count_agg" }) { id code } }"#,
        json!({}),
    )
    .await;
    data(&resp);
    let recent = db.recent_webhooks(org.id, 50).await.unwrap();
    assert!(!recent.iter().any(|(t, _, _)| t == "billable_metric.created"), "no endpoint → no webhook row");

    // Configure an endpoint; the next mutation records a pending webhook row with
    // the catalog's exact `billable_metric.created` type.
    db.insert_webhook_endpoint_returning(org.id, "https://hook.test/a").await.unwrap();
    let resp = run(
        &schema,
        &org,
        r#"mutation { createBillableMetric(input: { name: "Requests", code: "api_calls", aggregationType: "count_agg" }) { id code } }"#,
        json!({}),
    )
    .await;
    data(&resp);
    let recent = db.recent_webhooks(org.id, 50).await.unwrap();
    assert!(recent.iter().any(|(t, s, _)| t == "billable_metric.created" && s == "pending"), "endpoint set → billable_metric.created pending row: {recent:?}");
}

// ---- charges + charge filters ---------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn charge_and_filter_crud(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let p = plan(org.id, "starter");
    db.insert_plan(&p).await.unwrap();
    let met = metric(org.id, "api_calls");
    db.insert_billable_metric(&met).await.unwrap();

    // create charge
    let resp = run(
        &schema,
        &org,
        r#"mutation($plan: ID!, $bm: ID!) {
             createCharge(input: { planId: $plan, billableMetricId: $bm, chargeModel: "standard",
                                   properties: { amount: "0.10" } }) { id chargeModel }
           }"#,
        json!({ "plan": p.id.to_string(), "bm": met.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createCharge"]["chargeModel"], "standard");
    let charge_id = d["createCharge"]["id"].as_str().unwrap().to_string();
    assert_eq!(db.charges_for_plan(p.id).await.unwrap().len(), 1);

    // update charge
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateCharge(input: { id: $id, payInAdvance: true, minAmountCents: 100 }) { payInAdvance minAmountCents } }"#,
        json!({ "id": charge_id }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateCharge"]["payInAdvance"], true);
    assert_eq!(d["updateCharge"]["minAmountCents"], 100);

    // create filter
    let resp = run(
        &schema,
        &org,
        r#"mutation($cid: ID!) { createChargeFilter(input: { chargeId: $cid, properties: { region: "eu" }, invoiceDisplayName: "EU" }) { id invoiceDisplayName } }"#,
        json!({ "cid": charge_id }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createChargeFilter"]["invoiceDisplayName"], "EU");
    let filter_id = d["createChargeFilter"]["id"].as_str().unwrap().to_string();
    assert_eq!(db.charge_filters_for_charge(Uuid::parse_str(&charge_id).unwrap()).await.unwrap().len(), 1);

    // update filter
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateChargeFilter(input: { id: $id, invoiceDisplayName: "EU-West" }) { invoiceDisplayName } }"#,
        json!({ "id": filter_id }),
    )
    .await;
    assert_eq!(data(&resp)["updateChargeFilter"]["invoiceDisplayName"], "EU-West");

    // destroy filter + charge
    run(&schema, &org, r#"mutation($id: ID!) { destroyChargeFilter(id: $id) }"#, json!({ "id": filter_id })).await;
    assert_eq!(db.charge_filters_for_charge(Uuid::parse_str(&charge_id).unwrap()).await.unwrap().len(), 0);
    run(&schema, &org, r#"mutation($id: ID!) { destroyCharge(id: $id) }"#, json!({ "id": charge_id })).await;
    assert_eq!(db.charges_for_plan(p.id).await.unwrap().len(), 0);
}

// ---- billable metrics ------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_and_destroy_billable_metric(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let met = metric(org.id, "api_calls");
    db.insert_billable_metric(&met).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateBillableMetric(input: { id: $id, name: "Requests", aggregationType: "sum_agg", fieldName: "amount" }) { name aggregationType } }"#,
        json!({ "id": met.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateBillableMetric"]["aggregationType"], "sum_agg");
    let updated = db.find_billable_metric_opt(met.id).await.unwrap().unwrap();
    assert_eq!(updated.aggregation_type, AggregationType::SumAgg);

    run(&schema, &org, r#"mutation($id: ID!) { destroyBillableMetric(id: $id) }"#, json!({ "id": met.id.to_string() })).await;
    assert!(db.find_metric_by_code(org.id, "api_calls").await.unwrap().is_none());
}

// ---- add-ons ---------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_and_destroy_add_on(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let id = Uuid::new_v4();
    db.insert_add_on(id, org.id, "Setup", "setup", None, 5000, "USD").await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateAddOn(input: { id: $id, name: "Onboarding", amountCents: 9900 }) { name amountCents } }"#,
        json!({ "id": id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateAddOn"]["amountCents"], 9900);
    assert_eq!(db.find_add_on(id).await.unwrap().unwrap().4, 9900);

    run(&schema, &org, r#"mutation($id: ID!) { destroyAddOn(id: $id) }"#, json!({ "id": id.to_string() })).await;
    assert!(db.find_add_on(id).await.unwrap().is_none());
}

// ---- coupons + applied coupons --------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn coupon_and_applied_coupon(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let cp = coupon(org.id, "launch");
    db.insert_coupon(&cp).await.unwrap();
    let c = customer(org.id, "cust-ac");
    db.insert_customer(&c).await.unwrap();

    // update coupon
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateCoupon(input: { id: $id, name: "Launch50", amountCents: 750 }) { name amountCents } }"#,
        json!({ "id": cp.id.to_string() }),
    )
    .await;
    assert_eq!(data(&resp)["updateCoupon"]["amountCents"], 750);
    assert_eq!(db.find_coupon(cp.id).await.unwrap().unwrap().amount_cents, Some(750));

    // apply coupon to customer
    let resp = run(
        &schema,
        &org,
        r#"mutation($coupon: ID!, $customer: ID!) {
             createAppliedCoupon(couponId: $coupon, customerId: $customer) { id status }
           }"#,
        json!({ "coupon": cp.id.to_string(), "customer": c.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createAppliedCoupon"]["status"], "active");
    let applied_id = d["createAppliedCoupon"]["id"].as_str().unwrap().to_string();

    // terminate applied coupon
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { terminateAppliedCoupon(id: $id) { status } }"#,
        json!({ "id": applied_id }),
    )
    .await;
    assert_eq!(data(&resp)["terminateAppliedCoupon"]["status"], "terminated");
    let ac = db.find_applied_coupon(Uuid::parse_str(&applied_id).unwrap()).await.unwrap().unwrap();
    assert_eq!(ac.status, CouponStatus::Terminated);

    // destroy coupon
    run(&schema, &org, r#"mutation($id: ID!) { destroyCoupon(id: $id) }"#, json!({ "id": cp.id.to_string() })).await;
    assert!(db.find_coupon_by_code(org.id, "launch").await.unwrap().is_none());
}

// ---- invoices --------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn invoice_finalize_update_refresh_dispute(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-inv");
    db.insert_customer(&c).await.unwrap();
    let inv = draft_invoice(org.id, c.id);
    db.insert_invoice(&inv).await.unwrap();

    // seed a fee for the refresh recompute
    sqlx::query(
        "INSERT INTO fees (id, organization_id, billing_entity_id, invoice_id, amount_cents, amount_currency, taxes_amount_cents, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,'USD',$6, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(org.id)
    .bind(org.id)
    .bind(inv.id)
    .bind(1500i64)
    .bind(150i64)
    .execute(&db.pool)
    .await
    .unwrap();

    // finalize
    let resp = run(&schema, &org, r#"mutation($id: ID!) { finalizeInvoice(id: $id) { status } }"#, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["finalizeInvoice"]["status"], "finalized");
    assert_eq!(db.find_invoice(inv.id).await.unwrap().unwrap().status, InvoiceStatus::Finalized);

    // refresh totals from fees
    let resp = run(&schema, &org, r#"mutation($id: ID!) { refreshInvoice(id: $id) { feesAmountCents taxesAmountCents totalAmountCents } }"#, json!({ "id": inv.id.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["refreshInvoice"]["feesAmountCents"], 1500);
    assert_eq!(d["refreshInvoice"]["taxesAmountCents"], 150);
    assert_eq!(d["refreshInvoice"]["totalAmountCents"], 1650);

    // update payment status
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateInvoice(input: { id: $id, paymentStatus: "succeeded" }) { paymentStatus } }"#, json!({ "id": inv.id.to_string() })).await;
    assert_eq!(data(&resp)["updateInvoice"]["paymentStatus"], "succeeded");
    assert_eq!(db.find_invoice(inv.id).await.unwrap().unwrap().payment_status, PaymentStatus::Succeeded);

    // lose dispute
    let resp = run(&schema, &org, r#"mutation($id: ID!) { loseInvoiceDispute(id: $id) { id } }"#, json!({ "id": inv.id.to_string() })).await;
    data(&resp);
    let lost: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT payment_dispute_lost_at FROM invoices WHERE id = $1").bind(inv.id).fetch_one(&db.pool).await.unwrap();
    assert!(lost.is_some());
}

// ---- credit notes ----------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn credit_note_create_update_void(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-cn");
    db.insert_customer(&c).await.unwrap();
    let mut inv = draft_invoice(org.id, c.id);
    inv.status = InvoiceStatus::Finalized;
    db.insert_invoice(&inv).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($inv: ID!) {
             createCreditNote(input: { invoiceId: $inv, reason: "other", creditAmountCents: 500,
                                       items: [] }) { id creditAmountCents totalAmountCents }
           }"#,
        json!({ "inv": inv.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createCreditNote"]["creditAmountCents"], 500);
    let cn_id = d["createCreditNote"]["id"].as_str().unwrap().to_string();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateCreditNote(input: { id: $id, refundStatus: "succeeded" }) { id } }"#,
        json!({ "id": cn_id }),
    )
    .await;
    data(&resp);
    let cn = db.find_credit_note(Uuid::parse_str(&cn_id).unwrap()).await.unwrap().unwrap();
    assert_eq!(cn.refund_status, Some(1));

    let resp = run(&schema, &org, r#"mutation($id: ID!) { voidCreditNote(id: $id) { balanceAmountCents } }"#, json!({ "id": cn_id })).await;
    assert_eq!(data(&resp)["voidCreditNote"]["balanceAmountCents"], 0);
    let cn = db.find_credit_note(Uuid::parse_str(&cn_id).unwrap()).await.unwrap().unwrap();
    assert!(cn.voided_at.is_some());
}

// ---- wallets ---------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_customer_wallet(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-w");
    db.insert_customer(&c).await.unwrap();
    let now = Utc::now();
    let wallet = m::Wallet {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: c.id,
        status: WalletStatus::Active,
        name: Some("Credits".into()),
        rate_amount: Decimal::ONE,
        credits_balance: Decimal::ZERO,
        consumed_credits: Decimal::ZERO,
        balance_cents: 0,
        balance_currency: "USD".into(),
        consumed_amount_cents: 0,
        ongoing_balance_cents: 0,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 50,
        created_at: now,
        updated_at: now,
    };
    db.insert_wallet(&wallet).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateCustomerWallet(input: { id: $id, name: "Prepaid", priority: 10 }) { name } }"#,
        json!({ "id": wallet.id.to_string() }),
    )
    .await;
    assert_eq!(data(&resp)["updateCustomerWallet"]["name"], "Prepaid");
    let w = db.find_wallet(wallet.id).await.unwrap().unwrap();
    assert_eq!(w.name.as_deref(), Some("Prepaid"));
    assert_eq!(w.priority, 10);
}

// ---- subscriptions ---------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn update_subscription(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let c = customer(org.id, "cust-s");
    db.insert_customer(&c).await.unwrap();
    let p = plan(org.id, "starter");
    db.insert_plan(&p).await.unwrap();
    let now = Utc::now();
    let sub = m::Subscription {
        id: Uuid::new_v4(),
        organization_id: org.id,
        customer_id: c.id,
        plan_id: p.id,
        billing_entity_id: Some(org.id),
        external_id: "sub-1".into(),
        name: Some("Old".into()),
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(now),
        started_at: Some(now),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    };
    db.insert_subscription(&sub).await.unwrap();

    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateSubscription(input: { id: $id, name: "Renamed Sub" }) { name planCode } }"#,
        json!({ "id": sub.id.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateSubscription"]["name"], "Renamed Sub");
    assert_eq!(d["updateSubscription"]["planCode"], "starter");
    assert_eq!(db.find_subscription_opt(sub.id).await.unwrap().unwrap().name.as_deref(), Some("Renamed Sub"));
}

// ---- dunning + webhook endpoints + webhooks -------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn dunning_webhook_endpoint_and_retry(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    // dunning campaign
    let dc = Uuid::new_v4();
    db.insert_dunning_campaign(dc, org.id, "Default", "default", None, 3, 7, &json!([]), false).await.unwrap();
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateDunningCampaign(input: { id: $id, maxAttempts: 5, appliedToOrganization: true }) { maxAttempts appliedToOrganization } }"#,
        json!({ "id": dc.to_string() }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["updateDunningCampaign"]["maxAttempts"], 5);
    run(&schema, &org, r#"mutation($id: ID!) { destroyDunningCampaign(id: $id) }"#, json!({ "id": dc.to_string() })).await;
    assert!(db.find_dunning_campaign(dc).await.unwrap().is_none());

    // webhook endpoint
    let we = db.insert_webhook_endpoint_returning(org.id, "https://a.test/hook").await.unwrap();
    let resp = run(
        &schema,
        &org,
        r#"mutation($id: ID!) { updateWebhookEndpoint(input: { id: $id, webhookUrl: "https://b.test/hook" }) { webhookUrl } }"#,
        json!({ "id": we.to_string() }),
    )
    .await;
    assert_eq!(data(&resp)["updateWebhookEndpoint"]["webhookUrl"], "https://b.test/hook");
    run(&schema, &org, r#"mutation($id: ID!) { destroyWebhookEndpoint(id: $id) }"#, json!({ "id": we.to_string() })).await;
    assert!(db.find_webhook_endpoint(we).await.unwrap().is_none());

    // webhook retry
    let wh = db.insert_retrying_webhook(org.id, "invoice.created", "https://b.test/hook", &json!({})).await.unwrap();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { retryWebhook(id: $id) { id status } }"#, json!({ "id": wh.to_string() })).await;
    assert_eq!(data(&resp)["retryWebhook"]["status"], "retrying");
}

// ---- auth guard ------------------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn unauthenticated_admin_mutation_is_rejected(pool: PgPool) {
    let (schema, _db, _org) = setup(pool).await;
    // No Principal injected → gated resolver must error.
    let req = Request::new(r#"mutation { createTax(input: { name: "X", code: "x", rate: 1.0 }) { id } }"#);
    let resp = schema.execute(req).await;
    assert!(!resp.errors.is_empty());
    assert!(resp.errors[0].message.contains("unauthorized"));
}

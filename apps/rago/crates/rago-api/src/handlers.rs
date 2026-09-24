//! v1 resource handlers — wired to `rago-db` repositories and the `rago-billing`
//! engine. Request bodies use Lago's root-wrapped shape (`{ "customer": {…} }`);
//! responses use the same single-resource / collection envelopes.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Utc;
use rago_billing::aggregation::AggregationPeriod;
use rago_billing::dates::MonthlyDates;
use rago_billing::generate::{generate_subscription_invoice, ChargeInput};
use rago_billing::subscription_fee::Proration;
use rago_billing::wallets::{build_transaction, increase_balance, WalletCredit};
use rago_core::enums::*;
use rago_core::models::*;
use rago_core::money::Currency;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

// ---- request helpers -------------------------------------------------------

/// Extract the root-wrapped params object (`{ "customer": {…} }`), or the whole
/// body if not wrapped.
pub(crate) fn wrapped<'a>(body: &'a Value, root: &str) -> &'a Value {
    body.get(root).unwrap_or(body)
}
pub(crate) fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}
pub(crate) fn req_s(v: &Value, k: &str) -> ApiResult<String> {
    s(v, k).ok_or_else(|| ApiError::validation(k, "value_is_mandatory"))
}
pub(crate) fn i64_of(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}
pub(crate) fn bool_of(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}
pub(crate) fn dec_of(v: &Value, k: &str) -> Option<Decimal> {
    v.get(k).and_then(|x| match x {
        Value::String(s) => Decimal::from_str(s).ok(),
        Value::Number(n) => n.as_f64().and_then(Decimal::from_f64_retain),
        _ => None,
    })
}
pub(crate) fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

// ---- customers -------------------------------------------------------------

pub async fn create_customer(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "customer");
    let external_id = req_s(p, "external_id")?;

    // Upsert semantics (Customers::UpsertFromApiService).
    let existing = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_id)
        .await?;
    let was_update = existing.is_some();
    let customer = match existing {
        Some(mut c) => {
            c.name = s(p, "name").or(c.name);
            c.email = s(p, "email").or(c.email);
            c.country = s(p, "country").or(c.country);
            c.currency = s(p, "currency").or(c.currency);
            c.timezone = s(p, "timezone").or(c.timezone);
            c.tax_identification_number =
                s(p, "tax_identification_number").or(c.tax_identification_number);
            c.updated_at = now();
            st.db.update_customer(&c).await?;
            c
        }
        None => {
            let c = Customer {
                id: Uuid::new_v4(),
                organization_id: auth.organization.id,
                billing_entity_id: auth.organization.id,
                external_id,
                name: s(p, "name"),
                email: s(p, "email"),
                country: s(p, "country"),
                currency: s(p, "currency"),
                timezone: s(p, "timezone"),
                tax_identification_number: s(p, "tax_identification_number"),
                created_at: now(),
                updated_at: now(),
                deleted_at: None,
            };
            st.db.insert_customer(&c).await?;
            c
        }
    };
    // EU customers with a (new) VAT number are validated against VIES
    // (`customer.vies_check`, emitted by the queued job).
    rago_jobs::vies::enqueue_vies_check_logged(&st.db, customer.id).await;
    // `Customers::UpsertFromApiService` emits `customer.created` on insert,
    // `customer.updated` on update.
    let kind = if was_update {
        rago_integrations::webhooks::WebhookKind::CustomerUpdated
    } else {
        rago_integrations::webhooks::WebhookKind::CustomerCreated
    };
    crate::emit::webhook(
        &st.db,
        auth.organization.id,
        kind,
        json!({ "lago_id": customer.id, "external_id": customer.external_id, "name": customer.name, "email": customer.email }),
    )
    .await;
    Ok(Json(json!({ "customer": ser::customer(&customer) })))
}

pub async fn show_customer(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    Ok(Json(json!({ "customer": ser::customer(&c) })))
}

pub async fn list_customers(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let items = st
        .db
        .list_customers(auth.organization.id, pg.per_page(), pg.offset())
        .await?;
    let total = st.db.count_customers(auth.organization.id).await?;
    Ok(Json(json!({
        "customers": items.iter().map(ser::customer).collect::<Vec<_>>(),
        "meta": pagination::meta(pg.page(), pg.per_page(), total),
    })))
}

// ---- billable metrics ------------------------------------------------------

pub async fn create_billable_metric(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "billable_metric");
    let agg_str = req_s(p, "aggregation_type")?;
    let aggregation_type = aggregation_from_str(&agg_str)
        .ok_or_else(|| ApiError::validation("aggregation_type", "value_is_invalid"))?;
    let custom_aggregator = s(p, "custom_aggregator").filter(|c| !c.trim().is_empty());
    match (&aggregation_type, custom_aggregator.as_deref()) {
        (AggregationType::CustomAgg, None) => {
            return Err(ApiError::validation("custom_aggregator", "value_is_mandatory"))
        }
        (_, Some(src)) => rago_billing::aggregation::validate_custom_aggregator(src)
            .map_err(|_| ApiError::validation("custom_aggregator", "invalid_expression"))?,
        _ => {}
    }
    if matches!(aggregation_type, AggregationType::WeightedSumAgg)
        && s(p, "field_name").map_or(true, |f| f.trim().is_empty())
    {
        return Err(ApiError::validation("field_name", "value_is_mandatory"));
    }
    let m = BillableMetric {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        name: req_s(p, "name")?,
        code: req_s(p, "code")?,
        description: s(p, "description"),
        aggregation_type,
        field_name: s(p, "field_name"),
        recurring: bool_of(p, "recurring").unwrap_or(false),
        expression: s(p, "expression"),
        rounding_function: s(p, "rounding_function"),
        rounding_precision: i64_of(p, "rounding_precision").map(|v| v as i32),
        custom_aggregator,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    st.db.insert_billable_metric(&m).await?;
    Ok(Json(json!({ "billable_metric": ser::billable_metric(&m) })))
}

// ---- plans (with nested charges) -------------------------------------------

pub async fn create_plan(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "plan");
    let interval = s(p, "interval").and_then(interval_from_str);
    let plan = Plan {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        name: req_s(p, "name")?,
        code: req_s(p, "code")?,
        interval,
        description: s(p, "description"),
        amount_cents: i64_of(p, "amount_cents"),
        amount_currency: s(p, "amount_currency").unwrap_or_else(|| "USD".into()),
        trial_period: p.get("trial_period").and_then(Value::as_f64),
        pay_in_advance: bool_of(p, "pay_in_advance").unwrap_or(false),
        bill_charges_monthly: bool_of(p, "bill_charges_monthly"),
        parent_id: None,
        invoice_display_name: s(p, "invoice_display_name"),
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    st.db.insert_plan(&plan).await?;

    // Nested charges.
    let mut charges = Vec::new();
    if let Some(arr) = p.get("charges").and_then(Value::as_array) {
        for cp in arr {
            let metric_code = req_s(cp, "billable_metric_code")?;
            let metric = st
                .db
                .find_metric_by_code(auth.organization.id, &metric_code)
                .await?
                .ok_or_else(|| ApiError::NotFound("billable_metric".into()))?;
            let model_str = req_s(cp, "charge_model")?;
            let charge_model = charge_model_from_str(&model_str)
                .ok_or_else(|| ApiError::validation("charge_model", "value_is_invalid"))?;
            let charge = Charge {
                id: Uuid::new_v4(),
                organization_id: auth.organization.id,
                billable_metric_id: Some(metric.id),
                plan_id: Some(plan.id),
                code: metric.code.clone(),
                charge_model,
                properties: cp.get("properties").cloned().unwrap_or_else(|| json!({})),
                pay_in_advance: bool_of(cp, "pay_in_advance").unwrap_or(false),
                prorated: bool_of(cp, "prorated").unwrap_or(false),
                invoiceable: bool_of(cp, "invoiceable").unwrap_or(true),
                min_amount_cents: i64_of(cp, "min_amount_cents").unwrap_or(0),
                amount_currency: Some(plan.amount_currency.clone()),
                accepts_target_wallet: false,
                invoice_display_name: s(cp, "invoice_display_name"),
                parent_id: None,
                created_at: now(),
                updated_at: now(),
                deleted_at: None,
            };
            st.db.insert_charge(&charge).await?;
            charges.push(charge);
        }
    }

    let mut plan_json = ser::plan(&plan);
    plan_json["charges"] = json!(charges.iter().map(ser::charge).collect::<Vec<_>>());
    Ok(Json(json!({ "plan": plan_json })))
}

pub async fn show_plan(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let charges = st.db.charges_for_plan(plan.id).await?;
    let mut plan_json = ser::plan(&plan);
    plan_json["charges"] = json!(charges.iter().map(ser::charge).collect::<Vec<_>>());
    Ok(Json(json!({ "plan": plan_json })))
}

// ---- subscriptions ---------------------------------------------------------

pub async fn create_subscription(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "subscription");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let plan_code = req_s(p, "plan_code")?;

    let customer = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let plan = st
        .db
        .find_plan_by_code(auth.organization.id, &plan_code)
        .await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;

    let external_id = s(p, "external_id").unwrap_or_else(|| Uuid::new_v4().to_string());
    let billing_time = s(p, "billing_time")
        .and_then(billing_time_from_str)
        .unwrap_or(BillingTime::Calendar);
    let sub = Subscription {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        customer_id: customer.id,
        plan_id: plan.id,
        billing_entity_id: Some(auth.organization.id),
        external_id,
        name: s(p, "name"),
        status: SubscriptionStatus::Active,
        billing_time,
        subscription_at: Some(now()),
        started_at: Some(now()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now(),
        updated_at: now(),
    };
    st.db.insert_subscription(&sub).await?;
    let _ = st.db.insert_activity_log(
        auth.organization.id, "subscription.started", "Subscription", sub.id,
        Some(&external_customer_id), Some(&sub.external_id),
        &json!({ "plan_code": plan.code, "status": sub.status.as_str() }),
    ).await;
    Ok(Json(json!({
        "subscription": ser::subscription(&sub, &plan, &external_customer_id)
    })))
}

pub async fn terminate_subscription(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let body = terminate_subscription_now(&st, auth.organization.id, sub).await?;
    Ok(Json(json!({ "subscription": body })))
}

/// `Subscriptions::TerminateService`: a subscription that never started is
/// canceled (`subscription.canceled`); an active one is terminated
/// (`subscription.terminated`). Shared by the API and the customer portal.
pub(crate) async fn terminate_subscription_now(
    st: &AppState,
    organization_id: Uuid,
    mut sub: Subscription,
) -> ApiResult<Value> {
    let plan = st.db.find_plan(sub.plan_id).await?;
    let was_pending = matches!(sub.status, SubscriptionStatus::Pending);
    if was_pending {
        sub.status = SubscriptionStatus::Canceled;
    } else {
        sub.status = SubscriptionStatus::Terminated;
        sub.terminated_at = Some(now());
    }
    sub.canceled_at = Some(now());
    sub.updated_at = now();
    st.db.update_subscription(&sub).await?;

    let ext_customer = external_customer_id_for(st, sub.customer_id).await?;
    let body = ser::subscription(&sub, &plan, &ext_customer);
    let kind = if was_pending {
        rago_integrations::webhooks::WebhookKind::SubscriptionCanceled
    } else {
        rago_integrations::webhooks::WebhookKind::SubscriptionTerminated
    };
    crate::emit::webhook(&st.db, organization_id, kind, body.clone()).await;
    Ok(body)
}

async fn external_customer_id_for(st: &AppState, customer_id: Uuid) -> ApiResult<String> {
    Ok(st.db.customer_external_id(customer_id).await?.unwrap_or_default())
}

// ---- events ----------------------------------------------------------------

pub async fn create_event(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    use rago_billing::events::EventParams;

    let p = wrapped(&body, "event");
    let params = EventParams {
        code: req_s(p, "code")?,
        transaction_id: req_s(p, "transaction_id")?,
        external_subscription_id: req_s(p, "external_subscription_id")?,
        properties: p.get("properties").cloned().unwrap_or_else(|| json!({})),
        timestamp: p.get("timestamp").and_then(|t| t.as_f64().or_else(|| t.as_str().and_then(|s| s.parse().ok()))),
        precise_total_amount_cents: dec_of(p, "precise_total_amount_cents"),
    };

    // Shared validate + dedup + insert path (also used by reel usage ingest).
    match crate::emit::ingest_event(&st, auth.organization.id, params).await? {
        crate::emit::Ingest::Created(event) => Ok(Json(json!({ "event": ser::event(&event) }))),
        crate::emit::Ingest::Duplicate => Err(ApiError::validation(
            "transaction_id",
            "value_is_missing_or_already_exists",
        )),
    }
}

// ---- invoices (billing trigger + fetch) ------------------------------------

pub async fn bill_subscription(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let (mut invoice, fees) = compute_subscription_invoice(&st, &sub).await?;

    st.db.insert_numbered_invoice_with_fees(&mut invoice, &fees).await?;

    // Activity + audit log: the invoice was created/finalized during billing.
    let _ = st.db.insert_activity_log(
        auth.organization.id,
        "invoice.created",
        "Invoice",
        invoice.id,
        None,
        Some(&sub.external_id),
        &json!({ "number": invoice.number, "total_amount_cents": invoice.total_amount_cents, "status": invoice.status.as_str() }),
    ).await;
    let _ = st.db.insert_audit_log(
        auth.organization.id, "Invoice", invoice.id, "finalized",
        &json!({ "total_amount_cents": invoice.total_amount_cents }),
    ).await;

    // Invoice-finalized email through the injectable transport.
    let email = st.db.find_customer(sub.customer_id).await?.and_then(|c| c.email);
    crate::breadth::send_invoice_finalized(&st, &invoice, &fees, email.as_deref()).await;
    crate::observability::incr("invoices_generated");

    Ok(Json(json!({ "invoice": ser::invoice(&invoice, &fees) })))
}

pub async fn show_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(inv.id).await?;
    Ok(Json(json!({ "invoice": ser::invoice(&inv, &fees) })))
}

fn blank_invoice(sub: &Subscription, currency: &Currency) -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: sub.organization_id,
        billing_entity_id: sub.billing_entity_id.unwrap_or(sub.organization_id),
        customer_id: Some(sub.customer_id),
        // Assigned from the per-organization sequence at insert time.
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(now().date_naive()),
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
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

// ---- wallets ---------------------------------------------------------------

pub async fn create_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "wallet");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let customer = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let currency = s(p, "currency").unwrap_or_else(|| customer.currency.clone().unwrap_or_else(|| "USD".into()));
    let rate_amount = dec_of(p, "rate_amount").unwrap_or(Decimal::ONE);

    let mut wallet = Wallet {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        customer_id: customer.id,
        status: WalletStatus::Active,
        name: s(p, "name"),
        rate_amount,
        credits_balance: Decimal::ZERO,
        consumed_credits: Decimal::ZERO,
        balance_cents: 0,
        balance_currency: currency.clone(),
        consumed_amount_cents: 0,
        ongoing_balance_cents: 0,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 50,
        allowed_fee_types: Vec::new(),
        created_at: now(),
        updated_at: now(),
    };
    st.db.insert_wallet(&wallet).await?;
    crate::emit::webhook(
        &st.db,
        wallet.organization_id,
        rago_integrations::webhooks::WebhookKind::WalletCreated,
        ser::wallet(&wallet, &external_customer_id_for_wallet(&st, wallet.customer_id).await),
    )
    .await;

    // Optional initial grant (free, settled now) and purchase (`paid_credits`:
    // a pending transaction + credit invoice, settled once the invoice is paid).
    let granted = dec_of(p, "granted_credits").unwrap_or(Decimal::ZERO);
    let paid = dec_of(p, "paid_credits").unwrap_or(Decimal::ZERO);
    if granted < Decimal::ZERO || paid < Decimal::ZERO {
        return Err(ApiError::validation("credits", "value_must_be_positive"));
    }
    if granted > Decimal::ZERO {
        wallet = grant_credits(&st, &wallet, granted).await?;
    }
    if paid > Decimal::ZERO {
        rago_jobs::wallet_credits::purchase_paid_credits(&st.db, &wallet, paid)
            .await
            .map_err(job_error)?;
    }

    Ok(Json(json!({ "wallet": ser::wallet(&wallet, &external_customer_id) })))
}

pub async fn show_wallet(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let w = st.db.find_wallet(id).await?.ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let ext = external_customer_id_for(&st, w.customer_id).await?;
    let _ = auth;
    Ok(Json(json!({ "wallet": ser::wallet(&w, &ext) })))
}

pub async fn create_wallet_transaction(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "wallet_transaction");
    let wallet_id = Uuid::parse_str(&req_s(p, "wallet_id")?)
        .map_err(|_| ApiError::validation("wallet_id", "value_is_invalid"))?;
    let mut wallet = st
        .db
        .find_wallet(wallet_id)
        .await?
        .filter(|w| w.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;

    let paid = dec_of(p, "paid_credits").unwrap_or(Decimal::ZERO);
    let granted = dec_of(p, "granted_credits").unwrap_or(Decimal::ZERO);
    if paid < Decimal::ZERO || granted < Decimal::ZERO {
        return Err(ApiError::validation("credits", "value_must_be_positive"));
    }
    if paid + granted <= Decimal::ZERO {
        return Err(ApiError::validation("paid_credits", "value_is_mandatory"));
    }

    let mut transactions = Vec::new();
    if granted > Decimal::ZERO {
        let (updated, tx) = grant_credits_tx(&st, &wallet, granted).await?;
        wallet = updated;
        transactions.push(ser::wallet_transaction(&tx));
    }
    if paid > Decimal::ZERO {
        let (tx, _invoice) = rago_jobs::wallet_credits::purchase_paid_credits(&st.db, &wallet, paid)
            .await
            .map_err(job_error)?;
        transactions.push(ser::wallet_transaction(&tx));
    }

    Ok(Json(json!({ "wallet_transactions": transactions })))
}

/// Grant free credits: a settled inbound transaction applied to the balance.
async fn grant_credits(st: &AppState, wallet: &Wallet, credits: Decimal) -> ApiResult<Wallet> {
    grant_credits_tx(st, wallet, credits).await.map(|(w, _)| w)
}

async fn grant_credits_tx(
    st: &AppState,
    wallet: &Wallet,
    credits: Decimal,
) -> ApiResult<(Wallet, WalletTransaction)> {
    let cur = Currency::new(&wallet.balance_currency);
    let credit = WalletCredit::from_credit_amount(cur, wallet.rate_amount, credits, true);
    let tx = build_transaction(wallet, &credit, WalletTransactionType::Inbound, WalletTransactionStatus::Settled, None);
    st.db.insert_wallet_transaction(&tx).await?;
    let updated = increase_balance(wallet, &tx, false);
    st.db.update_wallet_balance(&updated).await?;
    crate::emit::webhook(
        &st.db,
        wallet.organization_id,
        rago_integrations::webhooks::WebhookKind::WalletTransactionCreated,
        rago_jobs::wallet_credits::wallet_transaction_object(&tx),
    )
    .await;
    Ok((updated, tx))
}

fn job_error(e: rago_jobs::JobError) -> ApiError {
    match e {
        rago_jobs::JobError::Db(e) => ApiError::from(e),
        rago_jobs::JobError::Billing(e) => ApiError::from(e),
        other => {
            tracing::error!(error = %other, "wallet credit purchase failed");
            ApiError::Internal("wallet_credit_purchase_failed".into())
        }
    }
}

// ---- coupons ---------------------------------------------------------------

pub async fn create_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "coupon");
    let coupon_type = s(p, "coupon_type").and_then(coupon_type_from_str).unwrap_or(CouponType::FixedAmount);
    let frequency = s(p, "frequency").and_then(coupon_frequency_from_str).unwrap_or(CouponFrequency::Once);
    let c = Coupon {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        name: req_s(p, "name")?,
        code: req_s(p, "code")?,
        status: CouponStatus::Active,
        coupon_type,
        amount_cents: i64_of(p, "amount_cents"),
        amount_currency: s(p, "amount_currency"),
        percentage_rate: dec_of(p, "percentage_rate"),
        frequency,
        frequency_duration: i64_of(p, "frequency_duration").map(|v| v as i32),
        expiration: CouponExpiration::NoExpiration,
        expiration_at: None,
        reusable: bool_of(p, "reusable").unwrap_or(true),
        limited_plans: false,
        limited_billable_metrics: false,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    st.db.insert_coupon(&c).await?;
    Ok(Json(json!({ "coupon": ser::coupon(&c) })))
}

// ---- credit notes ----------------------------------------------------------

pub async fn show_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    Ok(Json(json!({ "credit_note": ser::credit_note(&cn) })))
}

/// `POST /credit_notes/estimate` — estimate a credit note for an invoice's fees
/// (delegates to `rago_billing::credit_notes`).
pub async fn estimate_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    use rago_billing::credit_notes::{create_credit_note, CreditRefund};
    use rago_core::models::CreditNoteItem;

    let p = wrapped(&body, "credit_note");
    let invoice_id = Uuid::parse_str(&req_s(p, "invoice_id")?)
        .map_err(|_| ApiError::validation("invoice_id", "value_is_invalid"))?;
    let invoice = st.db.find_invoice(invoice_id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let mut fees = st.db.fees_for_invoice(invoice.id).await?;
    // attach applied taxes so the credit-note tax math has a base (fees carry none
    // when read back; re-derive from the invoice-level taxes is a follow-up).
    let items: Vec<CreditNoteItem> = p
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    let fee_id = Uuid::parse_str(it.get("fee_id")?.as_str()?).ok()?;
                    let amount = i64_of(it, "amount_cents")?;
                    Some(CreditNoteItem {
                        id: Uuid::new_v4(),
                        organization_id: invoice.organization_id,
                        credit_note_id: None,
                        fee_id,
                        amount_cents: amount,
                        precise_amount_cents: Decimal::from(amount),
                        amount_currency: invoice.currency().code(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return Err(ApiError::validation("items", "must_be_an_array"));
    }
    let _ = &mut fees;
    // Preview only (not persisted); project the number the created note would get.
    let seq = st.db.next_credit_note_sequential_id(invoice.id).await?;
    let cn = create_credit_note(&invoice, &fees, items, CreditRefund::CreditOnly, seq);
    Ok(Json(json!({ "estimated_credit_note": ser::credit_note(&cn) })))
}

/// `POST /credit_notes` — create and **persist** a credit note for an invoice's
/// fees (delegates to `rago_billing::credit_notes`, then persists).
pub async fn create_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    use rago_billing::credit_notes::{create_credit_note as build_cn, CreditRefund};
    use rago_core::models::CreditNoteItem;

    let p = wrapped(&body, "credit_note");
    let invoice_id = Uuid::parse_str(&req_s(p, "invoice_id")?)
        .map_err(|_| ApiError::validation("invoice_id", "value_is_invalid"))?;
    let invoice = st.db.find_invoice(invoice_id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(invoice.id).await?;

    let items: Vec<CreditNoteItem> = p
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    let fee_id = Uuid::parse_str(it.get("fee_id")?.as_str()?).ok()?;
                    let amount = i64_of(it, "amount_cents")?;
                    Some(CreditNoteItem {
                        id: Uuid::new_v4(),
                        organization_id: invoice.organization_id,
                        credit_note_id: None,
                        fee_id,
                        amount_cents: amount,
                        precise_amount_cents: Decimal::from(amount),
                        amount_currency: invoice.currency().code(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return Err(ApiError::validation("items", "must_be_an_array"));
    }

    let refund = match i64_of(p, "refund_amount_cents") {
        Some(r) if r > 0 => CreditRefund::Split { refund_amount_cents: r },
        _ => CreditRefund::CreditOnly,
    };
    let seq = st.db.next_credit_note_sequential_id(invoice.id).await?;
    let mut cn = build_cn(&invoice, &fees, items, refund, seq);
    cn.customer_id = invoice.customer_id.unwrap_or_default();
    if let Some(reason) = s(p, "reason").and_then(|r| credit_note_reason_i32(&r)) {
        cn.reason = reason;
    }
    st.db.insert_credit_note(&cn).await?;

    // `CreditNotes::CreateService` emits `credit_note.created`.
    crate::emit::webhook(
        &st.db,
        invoice.organization_id,
        rago_integrations::webhooks::WebhookKind::CreditNoteCreated,
        json!({ "lago_id": cn.id, "number": cn.number, "lago_invoice_id": cn.invoice_id, "total_amount_cents": cn.total_amount_cents }),
    )
    .await;
    let _ = rago_jobs::webhook_emit::record_credit_note_generated(&st.db, invoice.organization_id, cn.id, ser::credit_note(&cn)).await;

    // A refund portion is paid back through the provider that captured the
    // invoice's payment; the credit note's refund_status reflects the outcome.
    let refund = match crate::refunds::refund_credit_note_at_provider(&st, &invoice, &cn).await {
        Ok(Some(Ok(r))) => json!({ "status": r.state.as_str(), "provider": r.provider, "error": r.error }),
        Ok(Some(Err(rejection))) => {
            let _ = st.db.update_credit_note_refund_status(cn.id, 2).await;
            json!({ "status": "failed", "error": rejection.code() })
        }
        Ok(None) => Value::Null,
        Err(e) => {
            tracing::error!(error = %e, credit_note_id = %cn.id, "credit note refund failed");
            json!({ "status": "failed", "error": "refund_error" })
        }
    };
    let cn = st.db.find_credit_note(cn.id).await?.unwrap_or(cn);
    Ok(Json(json!({ "credit_note": ser::credit_note(&cn), "refund": refund })))
}

// ---- applied coupons -------------------------------------------------------

/// `POST /applied_coupons` — apply a coupon to a customer.
pub async fn create_applied_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "applied_coupon");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let coupon_code = req_s(p, "coupon_code")?;
    let customer = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let coupon = st
        .db
        .find_coupon_by_code(auth.organization.id, &coupon_code)
        .await?
        .ok_or_else(|| ApiError::NotFound("coupon".into()))?;

    let applied = AppliedCoupon {
        id: Uuid::new_v4(),
        organization_id: auth.organization.id,
        coupon_id: coupon.id,
        customer_id: customer.id,
        status: CouponStatus::Active,
        amount_cents: i64_of(p, "amount_cents").or(coupon.amount_cents),
        amount_currency: s(p, "amount_currency").or(coupon.amount_currency.clone()),
        percentage_rate: dec_of(p, "percentage_rate").or(coupon.percentage_rate),
        frequency: coupon.frequency,
        frequency_duration: coupon.frequency_duration,
        frequency_duration_remaining: coupon.frequency_duration,
        created_at: now(),
        updated_at: now(),
        terminated_at: None,
    };
    st.db.insert_applied_coupon(&applied).await?;
    Ok(Json(json!({
        "applied_coupon": {
            "lago_id": applied.id,
            "lago_coupon_id": coupon.id,
            "coupon_code": coupon.code,
            "external_customer_id": external_customer_id,
            "amount_cents": applied.amount_cents,
            "percentage_rate": applied.percentage_rate.map(|d| d.to_string()),
            "frequency": applied.frequency.as_str(),
            "status": applied.status.as_str(),
            "created_at": applied.created_at.to_rfc3339(),
        }
    })))
}

// ---- enum parsing ----------------------------------------------------------

/// Lago credit-note `reason` code → persisted integer.
fn credit_note_reason_i32(s: &str) -> Option<i32> {
    Some(match s {
        "duplicated_charge" => 0,
        "product_unsatisfactory" => 1,
        "order_change" => 2,
        "order_cancellation" => 3,
        "fraudulent_charge" => 4,
        "other" => 5,
        _ => return None,
    })
}

fn aggregation_from_str(s: &str) -> Option<AggregationType> {
    Some(match s {
        "count_agg" => AggregationType::CountAgg,
        "sum_agg" => AggregationType::SumAgg,
        "max_agg" => AggregationType::MaxAgg,
        "unique_count_agg" => AggregationType::UniqueCountAgg,
        "latest_agg" => AggregationType::LatestAgg,
        "weighted_sum_agg" => AggregationType::WeightedSumAgg,
        "custom_agg" => AggregationType::CustomAgg,
        _ => return None,
    })
}
fn charge_model_from_str(s: &str) -> Option<ChargeModel> {
    Some(match s {
        "standard" => ChargeModel::Standard,
        "graduated" => ChargeModel::Graduated,
        "package" => ChargeModel::Package,
        "percentage" => ChargeModel::Percentage,
        "volume" => ChargeModel::Volume,
        "graduated_percentage" => ChargeModel::GraduatedPercentage,
        "custom" => ChargeModel::Custom,
        "dynamic" => ChargeModel::Dynamic,
        _ => return None,
    })
}
fn interval_from_str(s: String) -> Option<PlanInterval> {
    Some(match s.as_str() {
        "weekly" => PlanInterval::Weekly,
        "monthly" => PlanInterval::Monthly,
        "yearly" => PlanInterval::Yearly,
        "quarterly" => PlanInterval::Quarterly,
        "semiannual" => PlanInterval::Semiannual,
        _ => return None,
    })
}
fn billing_time_from_str(s: String) -> Option<BillingTime> {
    Some(match s.as_str() {
        "calendar" => BillingTime::Calendar,
        "anniversary" => BillingTime::Anniversary,
        _ => return None,
    })
}
fn coupon_type_from_str(s: String) -> Option<CouponType> {
    Some(match s.as_str() {
        "fixed_amount" => CouponType::FixedAmount,
        "percentage" => CouponType::Percentage,
        _ => return None,
    })
}
fn coupon_frequency_from_str(s: String) -> Option<CouponFrequency> {
    Some(match s.as_str() {
        "once" => CouponFrequency::Once,
        "recurring" => CouponFrequency::Recurring,
        "forever" => CouponFrequency::Forever,
        _ => return None,
    })
}

// Small helper: next month (year, month) for building the billing_at anchor.
trait NextMonth {
    fn year_next_month(&self) -> (i32, u32);
}
impl NextMonth for chrono::NaiveDate {
    fn year_next_month(&self) -> (i32, u32) {
        use chrono::Datelike;
        if self.month() == 12 {
            (self.year() + 1, 1)
        } else {
            (self.year(), self.month() + 1)
        }
    }
}

// ---- refunds ---------------------------------------------------------------
//
// `POST /invoices/:id/refund` lives in [`crate::refunds`]: it refunds the
// captured payment at the provider instead of only flipping local status.

// ---- entitlements ----------------------------------------------------------

/// `POST /entitlements` — create a feature (+ privileges) and attach it to a
/// plan with values.
pub async fn create_entitlement(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let feature_code = req_s(&body, "feature_code")?;
    let plan_code = req_s(&body, "plan_code")?;
    let plan = st.db.find_plan_by_code(org, &plan_code).await?.ok_or_else(|| ApiError::NotFound("plan".into()))?;

    // Feature (idempotent by code).
    let feature_id = match st.db.find_feature_by_code(org, &feature_code).await? {
        Some(id) => id,
        None => {
            let id = Uuid::new_v4();
            st.db.insert_feature(id, org, &feature_code, s(&body, "name").as_deref(), s(&body, "description").as_deref()).await?;
            id
        }
    };

    // Privileges + values.
    let mut privileges: Vec<(String, Uuid)> = Vec::new();
    if let Some(arr) = body.get("privileges").and_then(Value::as_array) {
        for pv in arr {
            let code = req_s(pv, "code")?;
            let pid = Uuid::new_v4();
            let value_type = s(pv, "value_type").unwrap_or_else(|| "string".into());
            st.db.insert_privilege(pid, org, feature_id, &code, s(pv, "name").as_deref(), &value_type, &json!({})).await?;
            privileges.push((code, pid));
        }
    }

    // Attach the feature to the plan.
    let entitlement_id = Uuid::new_v4();
    st.db.insert_entitlement(entitlement_id, org, feature_id, plan.id).await?;

    // Values keyed by privilege code.
    if let Some(values) = body.get("values").and_then(Value::as_object) {
        for (code, val) in values {
            if let Some((_, pid)) = privileges.iter().find(|(c, _)| c == code) {
                let vstr = match val {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                st.db.insert_entitlement_value(Uuid::new_v4(), org, *pid, entitlement_id, &vstr).await?;
            }
        }
    }

    Ok(Json(json!({
        "entitlement": {
            "lago_id": entitlement_id,
            "feature_code": feature_code,
            "plan_code": plan_code,
            "privileges": privileges.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>(),
        }
    })))
}

/// `GET /plans/:code/entitlements` — the entitlements (features + values)
/// attached to a plan.
pub async fn list_plan_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let plan = st.db.find_plan_by_code(auth.organization.id, &code).await?
        .ok_or_else(|| ApiError::NotFound("plan".into()))?;
    let rows = st.db.plan_entitlements(plan.id).await?;

    // Group by feature code.
    let mut by_feature: std::collections::BTreeMap<String, (Option<String>, Vec<Value>)> = std::collections::BTreeMap::new();
    for (fcode, fname, pcode, value) in rows {
        let entry = by_feature.entry(fcode).or_insert((fname, Vec::new()));
        if let (Some(pc), Some(v)) = (pcode, value) {
            entry.1.push(json!({ "code": pc, "value": v }));
        }
    }
    let features: Vec<Value> = by_feature
        .into_iter()
        .map(|(code, (name, privileges))| json!({ "code": code, "name": name, "privileges": privileges }))
        .collect();

    Ok(Json(json!({ "plan_code": code, "entitlements": features })))
}

// ---- documents (PDF / XML) -------------------------------------------------

/// `GET /invoices/:id/download` — generate + return the invoice PDF.
pub async fn download_invoice_pdf(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(invoice.id).await?;
    let (name, ext) = match invoice.customer_id {
        Some(cid) => {
            let ext = st.db.customer_external_id(cid).await?.unwrap_or_default();
            (ext.clone(), ext)
        }
        None => (String::new(), String::new()),
    };
    let pdf = rago_documents::invoice_pdf(&invoice, &fees, &name, &ext);
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}.pdf\"", invoice.number),
        ),
    ];
    Ok((headers, pdf).into_response())
}

/// `GET /invoices/:id/download.xml` — the structured invoice XML.
pub async fn download_invoice_xml(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(invoice.id).await?;
    let ext = match invoice.customer_id {
        Some(cid) => st.db.customer_external_id(cid).await?.unwrap_or_default(),
        None => String::new(),
    };
    let xml = rago_documents::invoice_xml(&invoice, &fees, &ext);
    let headers = [(axum::http::header::CONTENT_TYPE, "application/xml".to_string())];
    Ok((headers, xml).into_response())
}

// ---- subscription entitlement resolution -----------------------------------

/// `POST /subscriptions/:external_id/entitlements` — add a subscription-level
/// entitlement override for a feature (value-type validated).
pub async fn create_subscription_entitlement(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st.db.find_subscription_by_external_id(org, &external_id).await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let feature_code = req_s(&body, "feature_code")?;
    let feature_id = st.db.find_feature_by_code(org, &feature_code).await?
        .ok_or_else(|| ApiError::NotFound("feature".into()))?;

    let entitlement_id = Uuid::new_v4();
    st.db.insert_subscription_entitlement(entitlement_id, org, feature_id, sub.id).await?;

    // Values (validated against the privilege value_type).
    if let Some(values) = body.get("values").and_then(Value::as_object) {
        for (code, val) in values {
            let vtype = st.db.privilege_value_type(org, feature_id, code).await?
                .ok_or_else(|| ApiError::NotFound("privilege".into()))?;
            let vstr = match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !valid_privilege_value(&vtype, &vstr) {
                return Err(ApiError::validation(code, "value_is_not_valid"));
            }
            // Look up privilege id.
            if let Some(pid) = st.db.privilege_id(org, feature_id, code).await? {
                st.db.insert_entitlement_value(Uuid::new_v4(), org, pid, entitlement_id, &vstr).await?;
            }
        }
    }
    Ok(Json(json!({ "entitlement": { "lago_id": entitlement_id, "feature_code": feature_code, "external_subscription_id": external_id } })))
}

/// `GET /subscriptions/:external_id/entitlements` — the resolved entitlement set
/// (plan entitlements + subscription overrides; overrides win).
pub async fn resolve_subscription_entitlements(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st.db.find_subscription_by_external_id(org, &external_id).await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;

    // Plan entitlements, then overlay subscription overrides.
    let plan_rows = st.db.plan_entitlements(sub.plan_id).await?;
    let sub_rows = st.db.subscription_entitlements(sub.id).await?;

    // feature_code -> (name, {privilege_code -> (value, source)})
    use std::collections::BTreeMap;
    type PrivilegeMap = BTreeMap<String, (String, String)>;
    type MergedEntitlements = BTreeMap<String, (Option<String>, PrivilegeMap)>;
    let mut merged: MergedEntitlements = BTreeMap::new();
    for (fc, fn_, pc, v) in plan_rows {
        let entry = merged.entry(fc).or_insert((fn_, BTreeMap::new()));
        if let (Some(pc), Some(v)) = (pc, v) {
            entry.1.insert(pc, (v, "plan".to_string()));
        }
    }
    for (fc, fn_, pc, v) in sub_rows {
        let entry = merged.entry(fc).or_insert((fn_, BTreeMap::new()));
        if let (Some(pc), Some(v)) = (pc, v) {
            entry.1.insert(pc, (v, "subscription".to_string())); // override wins
        }
    }

    let entitlements: Vec<Value> = merged.into_iter().map(|(code, (name, privs))| {
        json!({
            "code": code,
            "name": name,
            "privileges": privs.into_iter().map(|(pc, (v, src))| json!({ "code": pc, "value": v, "source": src })).collect::<Vec<_>>(),
        })
    }).collect();

    Ok(Json(json!({ "external_subscription_id": external_id, "entitlements": entitlements })))
}

fn valid_privilege_value(value_type: &str, value: &str) -> bool {
    match value_type {
        "integer" => value.parse::<i64>().is_ok(),
        "boolean" => matches!(value, "true" | "false"),
        "string" => true,
        _ => true,
    }
}

// ---- activity logs ---------------------------------------------------------

/// `GET /activity_logs` — recent activity-log entries.
pub async fn list_activity_logs(
    State(st): State<AppState>,
    auth: Authed,
    Query(pg): Query<PageParams>,
) -> ApiResult<Json<Value>> {
    let logs = st.db.list_activity_logs(auth.organization.id, pg.per_page()).await?;
    let total = st.db.count_activity_logs(auth.organization.id).await?;
    let collection: Vec<Value> = logs.into_iter().map(|(atype, rtype, rid, payload, created)| {
        json!({
            "activity_type": atype,
            "resource_type": rtype,
            "resource_id": rid,
            "payload": payload,
            "logged_at": created.to_rfc3339(),
        })
    }).collect();
    Ok(Json(json!({
        "activity_logs": collection,
        "meta": crate::pagination::meta(pg.page(), pg.per_page(), total),
    })))
}

/// External id of a wallet's customer (empty when unknown) for payloads.
async fn external_customer_id_for_wallet(st: &AppState, customer_id: Uuid) -> String {
    st.db.customer_external_id(customer_id).await.ok().flatten().unwrap_or_default()
}

/// Compute (without persisting) the current-period invoice of `sub`: the
/// subscription fee plus every charge over the period's events, taxed with the
/// organization's taxes. Shared by billing (`POST /subscriptions/:id/invoices`,
/// which persists it) and `POST /invoices/preview` (which does not).
pub(crate) async fn compute_subscription_invoice(
    st: &AppState,
    sub: &Subscription,
) -> ApiResult<(Invoice, Vec<Fee>)> {
    let plan = st.db.find_plan(sub.plan_id).await?;
    let currency = plan.currency();
    let charges = st.db.charges_for_plan(plan.id).await?;

    // Current calendar-month billing period.
    let sub_at = sub.subscription_at.unwrap_or_else(now).date_naive();
    let today = now().date_naive();
    let dates = MonthlyDates {
        subscription_at: sub_at,
        billing_at: chrono::NaiveDate::from_ymd_opt(today.year_next_month().0, today.year_next_month().1, 1).unwrap(),
        anniversary: matches!(sub.billing_time, BillingTime::Anniversary),
        pay_in_advance: plan.pay_in_advance,
        current_usage: false,
    };
    let b = dates.boundaries();
    let from = b.from_date.and_hms_opt(0, 0, 0).unwrap();
    let to = b.to_date.and_hms_opt(23, 59, 59).unwrap();

    // Load metrics + events per charge.
    let mut loaded: Vec<(BillableMetric, Charge, Vec<Event>, AggregationPeriod)> = Vec::new();
    for charge in &charges {
        // Pay-in-advance charges were billed when their events were ingested.
        if charge.pay_in_advance {
            continue;
        }
        let Some(mid) = charge.billable_metric_id else { continue };
        let metric = st.db.find_billable_metric(mid).await?;
        let events = st
            .db
            .events_for_external_subscription(sub.organization_id, &sub.external_id, &metric.code, from, to)
            .await?;
        let carry_in = st
            .db
            .weighted_sum_carry_in(sub.organization_id, &sub.external_id, &metric, from)
            .await?;
        let period = AggregationPeriod::new(from, to).with_initial_value(carry_in);
        loaded.push((metric, charge.clone(), events, period));
    }
    let charge_inputs: Vec<ChargeInput> = loaded
        .iter()
        .map(|(m, c, e, p)| ChargeInput { metric: m, charge: c, events: e, period: Some(*p) })
        .collect();

    // Organization taxes apply to the subscription + charge fees.
    let taxes = st.db.organization_taxes(sub.organization_id).await?;
    let mut invoice = blank_invoice(sub, &currency);
    let fees = generate_subscription_invoice(
        &mut invoice,
        &plan,
        sub,
        &dates,
        Proration::Full,
        &charge_inputs,
        &taxes,
        &[],
    )?;

    Ok((invoice, fees))
}

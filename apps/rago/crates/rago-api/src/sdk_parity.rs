//! Lago REST routes that SDK clients call and Rago previously answered with a
//! 404: quotes index/show, event lookup by transaction id, invoice update and
//! preview, fees index/show/update/destroy, applied-coupon removal, dunning
//! campaign show/update/destroy by code, wallet recurring rules, the org-wide
//! wallets index, and the operator "retry delivery" action for webhooks.
//!
//! Every handler is organization-scoped (plus the tenancy middleware).

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_core::enums::PaymentStatus;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, s, wrapped};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

fn payment_status_i32(v: &str) -> Option<i32> {
    Some(match v {
        "pending" => PaymentStatus::Pending,
        "succeeded" => PaymentStatus::Succeeded,
        "failed" => PaymentStatus::Failed,
        _ => return None,
    }
    .as_i32())
}

fn fee_type_i32(v: &str) -> Option<i32> {
    Some(match v {
        "charge" => 0,
        "add_on" => 1,
        "subscription" => 2,
        "credit" => 3,
        "commitment" => 4,
        "fixed_charge" => 5,
        _ => return None,
    })
}

// ---- quotes -----------------------------------------------------------------

fn quote_json(q: &rago_db::QuoteRow) -> Value {
    json!({
        "lago_id": q.id,
        "lago_customer_id": q.customer_id,
        "number": q.number,
        "status": q.status,
        "currency": q.currency,
        "total_amount_cents": q.total_amount_cents,
        "line_items": q.line_items,
    })
}

/// `GET /quotes`.
pub async fn list_quotes(State(st): State<AppState>, auth: Authed) -> ApiResult<Json<Value>> {
    let quotes = st.db.list_quotes(auth.organization.id).await?;
    Ok(Json(json!({ "quotes": quotes.iter().map(quote_json).collect::<Vec<_>>() })))
}

/// `GET /quotes/:id`.
pub async fn show_quote(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Ownership is enforced by the tenancy middleware (foreign ids → 404).
    let q = st
        .db
        .find_quote_full(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("quote".into()))?;
    Ok(Json(json!({ "quote": quote_json(&q) })))
}

// ---- events -------------------------------------------------------------------

/// `GET /events/:transaction_id`.
pub async fn show_event(
    State(st): State<AppState>,
    auth: Authed,
    Path(transaction_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let e = st
        .db
        .find_event_by_transaction_id(auth.organization.id, &transaction_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("event".into()))?;
    Ok(Json(json!({ "event": ser::event(&e) })))
}

// ---- invoices: update + preview ---------------------------------------------------

/// `PUT /invoices/:id` — update `payment_status` (manual collection) and/or
/// `metadata` (`[{ key, value }]`, merged).
pub async fn update_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "invoice");
    let payment_status = match s(p, "payment_status") {
        Some(v) => Some(
            payment_status_i32(&v)
                .ok_or_else(|| ApiError::validation("payment_status", "value_is_invalid"))?,
        ),
        None => None,
    };
    let metadata = p.get("metadata").and_then(Value::as_array).map(|items| {
        let mut m = Map::new();
        for it in items {
            if let (Some(k), Some(v)) = (it.get("key").and_then(Value::as_str), it.get("value")) {
                m.insert(k.to_string(), v.clone());
            }
        }
        Value::Object(m)
    });
    if !st.db.update_invoice_fields(org, id, payment_status, metadata.as_ref()).await? {
        return Err(ApiError::NotFound("invoice".into()));
    }
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(inv.id).await?;
    let meta = st.db.invoice_metadata(org, id).await?;
    let mut out = ser::invoice(&inv, &fees);
    out["metadata"] = Value::Array(
        meta.as_object()
            .map(|m| m.iter().map(|(k, v)| json!({ "key": k, "value": v })).collect())
            .unwrap_or_default(),
    );
    Ok(Json(json!({ "invoice": out })))
}

/// `POST /invoices/preview` — the invoice the given subscription(s) would
/// produce for the current period, computed with the billing engine and NOT
/// persisted. Body: `{ subscriptions: { external_ids: [..] } }` (Lago's
/// existing-subscription preview mode).
pub async fn preview_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let ids: Vec<String> = body
        .pointer("/subscriptions/external_ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(ApiError::validation("subscriptions", "external_ids_required"));
    }
    let mut previews = Vec::with_capacity(ids.len());
    let mut total = 0i64;
    let mut currency: Option<String> = None;
    for ext in &ids {
        let sub = st
            .db
            .find_subscription_by_external_id(org, ext)
            .await?
            .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
        let (invoice, fees) = crate::handlers::compute_subscription_invoice(&st, &sub).await?;
        total += invoice.total_amount_cents;
        currency = currency.or_else(|| invoice.currency.clone());
        let mut v = ser::invoice(&invoice, &fees);
        v["external_subscription_id"] = json!(ext);
        v["status"] = json!("preview");
        previews.push(v);
    }
    Ok(Json(json!({
        "invoice": {
            "status": "preview",
            "currency": currency,
            "total_amount_cents": total,
            "subscriptions": previews,
        }
    })))
}

// ---- fees -------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FeeIndexQuery {
    #[serde(flatten)]
    pub page: PageParams,
    pub fee_type: Option<String>,
    pub payment_status: Option<String>,
    pub external_subscription_id: Option<String>,
}

/// `GET /fees`.
pub async fn list_fees(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<FeeIndexQuery>,
) -> ApiResult<Json<Value>> {
    let (fees, total) = st
        .db
        .list_fees_for_org(
            auth.organization.id,
            q.fee_type.as_deref().and_then(fee_type_i32),
            q.payment_status.as_deref().and_then(payment_status_i32),
            q.external_subscription_id.as_deref(),
            q.page.per_page(),
            q.page.offset(),
        )
        .await?;
    Ok(Json(json!({
        "fees": fees.iter().map(ser::fee).collect::<Vec<_>>(),
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

async fn owned_fee(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<rago_core::models::Fee> {
    st.db
        .find_fee_opt(id)
        .await?
        .filter(|f| f.organization_id == org)
        .ok_or_else(|| ApiError::NotFound("fee".into()))
}

/// `GET /fees/:id`.
pub async fn show_fee(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let fee = owned_fee(&st, auth.organization.id, id).await?;
    Ok(Json(json!({ "fee": ser::fee(&fee) })))
}

/// `PUT /fees/:id` — update the fee's `payment_status` (pay-in-advance fees
/// collected outside Rago).
pub async fn update_fee(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "fee");
    let status = s(p, "payment_status")
        .as_deref()
        .and_then(payment_status_i32)
        .ok_or_else(|| ApiError::validation("payment_status", "value_is_invalid"))?;
    if !st.db.update_fee_payment_status(org, id, status).await? {
        return Err(ApiError::NotFound("fee".into()));
    }
    let fee = owned_fee(&st, org, id).await?;
    Ok(Json(json!({ "fee": ser::fee(&fee) })))
}

/// `DELETE /fees/:id` — only fees not on a finalized invoice can be deleted.
pub async fn destroy_fee(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let fee = owned_fee(&st, org, id).await?;
    if !st.db.delete_uninvoiced_fee(org, id).await? {
        return Err(ApiError::Validation(json!({ "fee": ["invoiced_fee"] })));
    }
    Ok(Json(json!({ "fee": ser::fee(&fee) })))
}

// ---- applied coupons ------------------------------------------------------------------

/// `DELETE /customers/:external_id/applied_coupons/:id` — terminate an applied
/// coupon of that customer.
pub async fn destroy_applied_coupon(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let ac = st
        .db
        .find_applied_coupon(id)
        .await?
        .filter(|a| a.organization_id == org && a.customer_id == customer.id)
        .ok_or_else(|| ApiError::NotFound("applied_coupon".into()))?;
    let terminated = st
        .db
        .terminate_applied_coupon(ac.id)
        .await?
        .ok_or_else(|| ApiError::NotFound("applied_coupon".into()))?;
    Ok(Json(json!({
        "applied_coupon": {
            "lago_id": terminated.id,
            "lago_coupon_id": terminated.coupon_id,
            "lago_customer_id": terminated.customer_id,
            "external_customer_id": external_id,
            "status": "terminated",
        }
    })))
}

// ---- dunning campaigns by code --------------------------------------------------------

fn campaign_json(c: &rago_db::DunningCampaignRow) -> Value {
    let (id, name, code, description, max_attempts, days_between, applied) = c;
    json!({
        "lago_id": id,
        "name": name,
        "code": code,
        "description": description,
        "max_attempts": max_attempts,
        "days_between_attempts": days_between,
        "applied_to_organization": applied,
    })
}

async fn campaign_by_code(st: &AppState, org: Uuid, code: &str) -> ApiResult<rago_db::DunningCampaignRow> {
    let id = st
        .db
        .dunning_campaign_id_by_code(org, code)
        .await?
        .ok_or_else(|| ApiError::NotFound("dunning_campaign".into()))?;
    st.db
        .find_dunning_campaign(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("dunning_campaign".into()))
}

/// `GET /dunning_campaigns/:code`.
pub async fn show_dunning_campaign(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = campaign_by_code(&st, auth.organization.id, &code).await?;
    Ok(Json(json!({ "dunning_campaign": campaign_json(&c) })))
}

/// `PUT /dunning_campaigns/:code`.
pub async fn update_dunning_campaign(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let c = campaign_by_code(&st, auth.organization.id, &code).await?;
    let p = wrapped(&body, "dunning_campaign");
    let updated = st
        .db
        .update_dunning_campaign(
            c.0,
            s(p, "name").as_deref(),
            s(p, "code").as_deref(),
            s(p, "description").as_deref(),
            i64_of(p, "max_attempts").map(|v| v as i32),
            i64_of(p, "days_between_attempts").map(|v| v as i32),
            p.get("applied_to_organization").and_then(Value::as_bool),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound("dunning_campaign".into()))?;
    Ok(Json(json!({ "dunning_campaign": campaign_json(&updated) })))
}

/// `DELETE /dunning_campaigns/:code`.
pub async fn destroy_dunning_campaign(
    State(st): State<AppState>,
    auth: Authed,
    Path(code): Path<String>,
) -> ApiResult<Json<Value>> {
    let c = campaign_by_code(&st, auth.organization.id, &code).await?;
    st.db.delete_dunning_campaign(c.0).await?;
    Ok(Json(json!({ "dunning_campaign": campaign_json(&c) })))
}

// ---- wallets --------------------------------------------------------------------------

/// `GET /wallets/:id/recurring_rules`.
pub async fn list_wallet_recurring_rules(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rules = st.db.wallet_recurring_rules(auth.organization.id, id).await?;
    let out: Vec<Value> = rules
        .into_iter()
        .map(|(rid, rule_type, interval, threshold, paid, granted)| {
            json!({
                "lago_id": rid,
                "trigger": rule_type,
                "interval": interval,
                "threshold_credits": threshold.map(|d| d.to_string()),
                "paid_credits": paid.to_string(),
                "granted_credits": granted.to_string(),
            })
        })
        .collect();
    Ok(Json(json!({ "recurring_transaction_rules": out })))
}

#[derive(Debug, Deserialize)]
pub struct WalletsQuery {
    pub external_customer_id: Option<String>,
    #[serde(flatten)]
    pub page: PageParams,
}

/// `GET /wallets` — a customer's wallets (`?external_customer_id=`, Lago), or
/// every wallet of the organization, paginated.
pub async fn list_wallets(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<WalletsQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    if let Some(ext) = q.external_customer_id.filter(|e| !e.is_empty()) {
        let c = st
            .db
            .find_customer_by_external_id(org, &ext)
            .await?
            .ok_or_else(|| ApiError::NotFound("customer".into()))?;
        let wallets = st.db.wallets_for_customer(c.id).await?;
        let total = wallets.len() as i64;
        return Ok(Json(json!({
            "wallets": wallets.iter().map(|w| ser::wallet(w, &ext)).collect::<Vec<_>>(),
            "meta": pagination::meta(1, total.max(1), total),
        })));
    }
    let (rows, total) = st.db.list_org_wallets(org, q.page.per_page(), q.page.offset()).await?;
    Ok(Json(json!({
        "wallets": rows.iter().map(|(w, ext)| ser::wallet(w, ext)).collect::<Vec<_>>(),
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

// ---- webhook deliveries: retry --------------------------------------------------------

/// `POST /webhook_deliveries/:id/retry` — re-queue a failed delivery; the
/// delivery worker sends it on its next tick.
pub async fn retry_webhook_delivery(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let webhook_id = st
        .db
        .webhook_id_for_delivery(org, id)
        .await?
        .ok_or_else(|| ApiError::Validation(json!({ "webhook_delivery": ["not_retryable"] })))?;
    if !st.db.requeue_webhook(org, webhook_id).await? {
        return Err(ApiError::NotFound("webhook_delivery".into()));
    }
    Ok(Json(json!({ "webhook_delivery": { "lago_id": id, "status": "pending", "requeued": true } })))
}

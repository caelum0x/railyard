//! REST surface for the quote → order form → order flow (Lago's
//! `Quotes::*`, `OrderForms::*` and `Orders::*` services), used by API clients
//! and the operator console's Quotes / Orders pages.
//!
//! * Quotes: list / show (with versions, order forms and orders), void.
//! * Versions: edit a draft's line items (re-priced with the org's taxes),
//!   approve (superseding any previously approved version), void, and start a
//!   new draft from an existing version.
//! * Order forms: issue from the approved version, mark signed (which creates
//!   the order and, with `execution_mode = "immediate"`, executes it), void.
//! * Orders: list / show, execute (bills the quote on a one-off invoice exactly
//!   once), void a pending order.
//!
//! Every lookup is scoped to the caller's organization (plus the tenancy
//! middleware), and each transition is guarded on the current status so a
//! voided / signed / executed record can never move backwards.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{NaiveDate, NaiveDateTime};
use rago_core::money::Currency;
use rago_db::order_flow_console::{OrderDetail, OrderFormDetail, QuoteDetail, QuoteVersionDetail};
use rago_integrations::webhooks::WebhookKind;
use rago_jobs::orders::OrderFlowError;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{s, wrapped};
use crate::pagination;
use crate::state::AppState;

/// Execution modes accepted when signing an order form.
const EXECUTION_MODES: &[&str] = &["manual", "immediate"];

fn ts(v: Option<NaiveDateTime>) -> Value {
    v.map(|t| json!(t.and_utc().to_rfc3339())).unwrap_or(Value::Null)
}

fn quote_json(q: &QuoteDetail) -> Value {
    json!({
        "lago_id": q.id,
        "lago_customer_id": q.customer_id,
        "external_customer_id": q.customer_external_id,
        "customer_name": q.customer_name,
        "number": q.number,
        "status": q.status,
        "currency": q.currency,
        "sub_total_excluding_taxes_amount_cents": q.sub_total_amount_cents,
        "taxes_amount_cents": q.taxes_amount_cents,
        "total_amount_cents": q.total_amount_cents,
        "line_items": q.line_items,
        "accepted_invoice_id": q.accepted_invoice_id,
        "versions_count": q.versions_count,
        "latest_version_status": q.latest_version_status,
        "created_at": ts(Some(q.created_at)),
        "updated_at": ts(Some(q.updated_at)),
    })
}

fn version_json(v: &QuoteVersionDetail) -> Value {
    json!({
        "lago_id": v.id,
        "lago_quote_id": v.quote_id,
        "version_number": v.version_number,
        "status": v.status,
        "currency": v.currency,
        "total_amount_cents": v.total_amount_cents,
        "line_items": v.line_items,
        "expires_at": ts(v.expires_at),
        "approved_at": ts(v.approved_at),
        "voided_at": ts(v.voided_at),
        "created_at": ts(Some(v.created_at)),
    })
}

fn order_form_json(f: &OrderFormDetail) -> Value {
    json!({
        "lago_id": f.id,
        "lago_customer_id": f.customer_id,
        "external_customer_id": f.customer_external_id,
        "customer_name": f.customer_name,
        "lago_quote_id": f.quote_id,
        "quote_number": f.quote_number,
        "number": f.number,
        "status": f.status,
        "execution_mode": f.execution_mode,
        "signed_document": f.signed_document,
        "signed_at": ts(f.signed_at),
        "lago_order_id": f.order_id,
        "created_at": ts(Some(f.created_at)),
    })
}

fn order_json(o: &OrderDetail) -> Value {
    json!({
        "lago_id": o.id,
        "lago_customer_id": o.customer_id,
        "external_customer_id": o.customer_external_id,
        "customer_name": o.customer_name,
        "lago_order_form_id": o.order_form_id,
        "order_form_number": o.order_form_number,
        "lago_quote_id": o.quote_id,
        "quote_number": o.quote_number,
        "number": o.number,
        "status": o.status,
        "executed_at": ts(o.executed_at),
        "lago_invoice_id": o.invoice_id,
        "invoice_total_amount_cents": o.total_amount_cents,
        "invoice_currency": o.currency,
        "created_at": ts(Some(o.created_at)),
    })
}

/// Map an order-flow refusal to the API envelope.
fn flow_error(e: OrderFlowError) -> ApiError {
    match e {
        OrderFlowError::Invalid(code) if code.ends_with("_not_found") => {
            ApiError::NotFound(code.trim_end_matches("_not_found").to_string())
        }
        OrderFlowError::Invalid(code) => ApiError::Validation(json!({ "order_flow": [code] })),
        OrderFlowError::Job(rago_jobs::JobError::Db(e)) => ApiError::from(e),
        OrderFlowError::Job(other) => {
            tracing::error!(error = %other, "order flow step failed");
            ApiError::Internal("order_flow_failed".into())
        }
    }
}

/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM[:SS]` (UTC) or RFC 3339.
fn parse_datetime(field: &str, raw: &str) -> ApiResult<NaiveDateTime> {
    let raw = raw.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(t.naive_utc());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Ok(t);
        }
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(23, 59, 59))
        .ok_or_else(|| ApiError::validation(field, "invalid_date"))
}

async fn load_quote(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<QuoteDetail> {
    st.db.quote_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("quote".into()))
}

async fn load_version(st: &AppState, org: Uuid, quote_id: Uuid, version_id: Uuid) -> ApiResult<QuoteVersionDetail> {
    st.db
        .quote_version_detail(org, version_id)
        .await?
        .filter(|v| v.quote_id == quote_id)
        .ok_or_else(|| ApiError::NotFound("quote_version".into()))
}

fn ensure_quote_open(q: &QuoteDetail) -> ApiResult<()> {
    if q.status == "draft" {
        Ok(())
    } else {
        Err(ApiError::validation("quote", &format!("not_editable_when_{}", q.status)))
    }
}

// ---- quotes -------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct QuoteListParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub status: Option<String>,
    pub external_customer_id: Option<String>,
}

/// `GET /quotes` — the org's quotes (newest first), filterable by `status` and
/// `external_customer_id`, paginated.
pub async fn list_quotes(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<QuoteListParams>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let page = pagination::PageParams { page: q.page, per_page: q.per_page };
    let customer_id = match q.external_customer_id.as_deref().filter(|v| !v.is_empty()) {
        Some(ext) => match st.db.find_customer_by_external_id(org, ext).await? {
            Some(c) => Some(c.id),
            None => {
                return Ok(Json(json!({ "quotes": [], "meta": pagination::meta(page.page(), page.per_page(), 0) })));
            }
        },
        None => None,
    };
    let status = q.status.as_deref().filter(|v| !v.is_empty());
    let (rows, total) = st.db.quote_details(org, status, customer_id, page.per_page(), page.offset()).await?;
    Ok(Json(json!({
        "quotes": rows.iter().map(quote_json).collect::<Vec<_>>(),
        "meta": pagination::meta(page.page(), page.per_page(), total),
    })))
}

/// `GET /quotes/:id` — a quote with its versions, order forms and orders.
pub async fn show_quote(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    let versions = st.db.quote_version_details(org, id).await?;
    let forms = st.db.order_form_details(org, Some(id)).await?;
    let orders = st.db.order_details(org, None, Some(id)).await?;
    let mut body = quote_json(&quote);
    body["versions"] = json!(versions.iter().map(version_json).collect::<Vec<_>>());
    body["order_forms"] = json!(forms.iter().map(order_form_json).collect::<Vec<_>>());
    body["orders"] = json!(orders.iter().map(order_json).collect::<Vec<_>>());
    Ok(Json(json!({ "quote": body })))
}

/// `POST /quotes/:id/void` — void an open quote, its open versions and any
/// unsigned order form issued from it.
pub async fn void_quote(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    if !st.db.void_open_quote(org, id).await? {
        return Err(ApiError::validation("quote", &format!("not_voidable_when_{}", quote.status)));
    }
    crate::emit::webhook(&st.db, org, WebhookKind::QuoteVoided, json!({ "lago_id": id, "number": quote.number })).await;
    Ok(Json(json!({ "quote": quote_json(&load_quote(&st, org, id).await?) })))
}

// ---- versions -----------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct NewVersionBody {
    /// The version to copy; defaults to the latest version.
    pub from_version_id: Option<Uuid>,
}

/// `POST /quotes/:id/versions` — start a new draft version, copied from
/// `from_version_id` (default: the latest version).
pub async fn create_version(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    body: Option<Json<NewVersionBody>>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    ensure_quote_open(&quote)?;
    let versions = st.db.quote_version_details(org, id).await?;
    if versions.iter().any(|v| v.status == "draft") {
        return Err(ApiError::validation("quote", "already_has_a_draft_version"));
    }
    let from = body.and_then(|Json(b)| b.from_version_id);
    let created = match from.or_else(|| versions.last().map(|v| v.id)) {
        Some(src) => {
            load_version(&st, org, id, src).await?;
            st.db.clone_quote_version(src).await?.ok_or_else(|| ApiError::NotFound("quote_version".into()))?
        }
        // A quote created before versioning: start v1 from its line items.
        None => {
            st.db
                .insert_quote_version(Uuid::new_v4(), org, id, 1, &quote.currency, quote.total_amount_cents, &quote.line_items)
                .await?
        }
    };
    let version = load_version(&st, org, id, created.id).await?;
    Ok(Json(json!({ "quote_version": version_json(&version) })))
}

/// `PUT /quotes/:id/versions/:version_id` — replace a draft version's line
/// items (`[{ name, unit_amount_cents, units }]` or `[{ add_on_code, units }]`);
/// the total is re-priced with the org's taxes.
pub async fn update_version(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    ensure_quote_open(&quote)?;
    let version = load_version(&st, org, id, version_id).await?;
    if version.status != "draft" {
        return Err(ApiError::validation("quote_version", "only_draft_versions_can_be_edited"));
    }
    let p = wrapped(&body, "quote_version");
    let raw = p
        .get("line_items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ApiError::validation("line_items", "must_be_an_array"))?;
    let currency = Currency::new(&version.currency);
    let priced = crate::breadth::price_quote_lines(&st, org, quote.customer_id, &currency, &raw).await?;
    st.db
        .update_quote_version(version_id, &priced.line_items, priced.total_cents)
        .await?
        .filter(|v| v.status == "draft")
        .ok_or_else(|| ApiError::validation("quote_version", "only_draft_versions_can_be_edited"))?;
    let version = load_version(&st, org, id, version_id).await?;
    Ok(Json(json!({ "quote_version": version_json(&version) })))
}

/// `POST /quotes/:id/versions/:version_id/approve` — approve a draft version
/// (optional `expires_at`); a previously approved version is superseded.
pub async fn approve_version(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    ensure_quote_open(&quote)?;
    let body = body.map(|Json(b)| b).unwrap_or(Value::Null);
    let expires_at = match s(wrapped(&body, "quote_version"), "expires_at") {
        Some(raw) => {
            let at = parse_datetime("expires_at", &raw)?;
            if at <= chrono::Utc::now().naive_utc() {
                return Err(ApiError::validation("expires_at", "must_be_in_the_future"));
            }
            Some(at)
        }
        None => None,
    };
    let draft = load_version(&st, org, id, version_id).await?;
    let raw = draft.line_items.as_array().cloned().unwrap_or_default();
    let priced =
        crate::breadth::price_quote_lines(&st, org, quote.customer_id, &Currency::new(&draft.currency), &raw).await?;
    if !st.db.approve_draft_quote_version(version_id, expires_at).await? {
        return Err(ApiError::validation("quote_version", "only_draft_versions_can_be_approved"));
    }
    st.db.set_quote_amounts(id, priced.sub_total_cents, priced.taxes_cents, priced.total_cents).await?;
    crate::emit::webhook(
        &st.db,
        org,
        WebhookKind::QuoteApproved,
        json!({ "lago_id": version_id, "lago_quote_id": id, "number": quote.number }),
    )
    .await;
    let version = load_version(&st, org, id, version_id).await?;
    Ok(Json(json!({ "quote_version": version_json(&version) })))
}

/// `POST /quotes/:id/versions/:version_id/void` — void a version.
pub async fn void_version(
    State(st): State<AppState>,
    auth: Authed,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    ensure_quote_open(&quote)?;
    load_version(&st, org, id, version_id).await?;
    if !st.db.void_open_quote_version(version_id).await? {
        return Err(ApiError::validation("quote_version", "already_voided"));
    }
    crate::emit::webhook(
        &st.db,
        org,
        WebhookKind::QuoteVoided,
        json!({ "lago_id": version_id, "lago_quote_id": id, "number": quote.number }),
    )
    .await;
    let version = load_version(&st, org, id, version_id).await?;
    Ok(Json(json!({ "quote_version": version_json(&version) })))
}

// ---- order forms --------------------------------------------------------------

/// `POST /quotes/:id/order_forms` — issue the approved version as an order form
/// (idempotent: returns the open form when one exists).
pub async fn create_order_form(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let quote = load_quote(&st, org, id).await?;
    ensure_quote_open(&quote)?;
    let form = rago_jobs::orders::create_order_form(&st.db, org, id).await.map_err(flow_error)?;
    let detail = st.db.order_form_detail(org, form.0).await?.ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    Ok(Json(json!({ "order_form": order_form_json(&detail) })))
}

#[derive(Debug, Deserialize)]
pub struct OrderFormListParams {
    pub quote_id: Option<Uuid>,
}

/// `GET /order_forms` — the org's order forms (optionally for one quote).
pub async fn list_order_forms(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<OrderFormListParams>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.order_form_details(auth.organization.id, q.quote_id).await?;
    Ok(Json(json!({ "order_forms": rows.iter().map(order_form_json).collect::<Vec<_>>() })))
}

/// `GET /order_forms/:id`.
pub async fn show_order_form(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let form = st
        .db
        .order_form_detail(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    Ok(Json(json!({ "order_form": order_form_json(&form) })))
}

/// `POST /order_forms/:id/sign` — mark an order form signed (`signed_document`
/// reference, `execution_mode` `manual` | `immediate`). Signing creates the
/// order; `immediate` also executes it (bills the quote).
pub async fn sign_order_form(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let form = st.db.order_form_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    if form.status != "draft" {
        return Err(ApiError::validation("order_form", &format!("not_signable_when_{}", form.status)));
    }
    let body = body.map(|Json(b)| b).unwrap_or(Value::Null);
    let p = wrapped(&body, "order_form");
    let mode = s(p, "execution_mode").unwrap_or_else(|| "manual".into());
    if !EXECUTION_MODES.contains(&mode.as_str()) {
        return Err(ApiError::validation("execution_mode", "invalid_value"));
    }
    let document = s(p, "signed_document");
    st.db
        .mark_order_form_signed(id, document.as_deref(), Some(&mode), None)
        .await?
        .ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    crate::emit::webhook(&st.db, org, WebhookKind::OrderFormSigned, json!({ "lago_id": id, "number": form.number })).await;
    let order = rago_jobs::orders::create_order_for_signed_form(&st.db, org, id).await.map_err(flow_error)?;
    let mut invoice_id = None;
    if mode == "immediate" {
        let (_, invoice) = rago_jobs::orders::execute_order(&st.db, org, order.0).await.map_err(flow_error)?;
        invoice_id = Some(invoice.id);
    }
    let form = st.db.order_form_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    let order = st.db.order_detail(org, order.0).await?.ok_or_else(|| ApiError::NotFound("order".into()))?;
    Ok(Json(json!({
        "order_form": order_form_json(&form),
        "order": order_json(&order),
        "lago_invoice_id": invoice_id,
    })))
}

/// `POST /order_forms/:id/void` — void an unsigned order form.
pub async fn void_order_form(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let form = st.db.order_form_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    if !st.db.void_draft_order_form(org, id).await? {
        return Err(ApiError::validation("order_form", &format!("not_voidable_when_{}", form.status)));
    }
    crate::emit::webhook(&st.db, org, WebhookKind::OrderFormVoided, json!({ "lago_id": id, "number": form.number })).await;
    let form = st.db.order_form_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order_form".into()))?;
    Ok(Json(json!({ "order_form": order_form_json(&form) })))
}

// ---- orders -------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OrderListParams {
    pub status: Option<String>,
    pub quote_id: Option<Uuid>,
}

/// `GET /orders` — the org's orders, filterable by `status` / `quote_id`.
pub async fn list_orders(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<OrderListParams>,
) -> ApiResult<Json<Value>> {
    let status = q.status.as_deref().filter(|v| !v.is_empty());
    let rows = st.db.order_details(auth.organization.id, status, q.quote_id).await?;
    Ok(Json(json!({ "orders": rows.iter().map(order_json).collect::<Vec<_>>() })))
}

/// `GET /orders/:id`.
pub async fn show_order(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let order = st
        .db
        .order_detail(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("order".into()))?;
    Ok(Json(json!({ "order": order_json(&order) })))
}

/// `POST /orders/:id/execute` — bill a pending order's quote on a one-off
/// invoice (exactly once) and queue its collection.
pub async fn execute_order(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (_, invoice) = rago_jobs::orders::execute_order(&st.db, org, id).await.map_err(flow_error)?;
    let order = st.db.order_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order".into()))?;
    Ok(Json(json!({ "order": order_json(&order), "lago_invoice_id": invoice.id })))
}

/// `POST /orders/:id/void` — cancel a pending order.
pub async fn void_order(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let order = st.db.order_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order".into()))?;
    if !st.db.void_pending_order(org, id).await? {
        return Err(ApiError::validation("order", &format!("not_voidable_when_{}", order.status)));
    }
    let order = st.db.order_detail(org, id).await?.ok_or_else(|| ApiError::NotFound("order".into()))?;
    Ok(Json(json!({ "order": order_json(&order) })))
}

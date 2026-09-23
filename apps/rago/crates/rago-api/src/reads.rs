//! Operator read surfaces: the webhook deliveries feed, a wallet's transaction
//! ledger, and the dunning overview.
//!
//! Every handler is Bearer-authenticated and org-scoped through [`Authed`] and
//! reuses an existing [`rago_db::Db`] read — no data bypass. Responses follow
//! the Lago-style `{ "<root>": [...], "meta": {...} }` collection envelope.

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_db::WebhookDeliveryRow;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

fn rfc3339_opt(dt: Option<chrono::DateTime<chrono::Utc>>) -> Value {
    dt.map(|d| json!(d.to_rfc3339())).unwrap_or(Value::Null)
}

// ---- webhook deliveries ----------------------------------------------------

/// One delivery-feed row (there is no Lago serializer for this table — it is a
/// Rago read surface — so the envelope is defined here).
fn delivery_json(d: &WebhookDeliveryRow) -> Value {
    json!({
        "lago_id": d.id,
        "webhook_endpoint_id": d.endpoint_id,
        "endpoint": d.endpoint,
        "event_type": d.event_type,
        "object_type": d.object_type,
        "status": d.status,
        "http_status": d.http_status,
        "attempts": d.attempts,
        "last_error": d.last_error,
        "payload": d.payload,
        "created_at": d.created_at.to_rfc3339(),
        "delivered_at": rfc3339_opt(d.delivered_at),
    })
}

#[derive(Debug, Deserialize)]
pub struct WebhookDeliveryIndexQuery {
    /// Filter by the configured endpoint id.
    pub webhook_endpoint_id: Option<Uuid>,
    /// Filter by webhook event/type (e.g. `invoice.created`).
    pub event_type: Option<String>,
    /// Filter by delivery status (`pending` / `succeeded` / `failed` / `retrying`).
    pub status: Option<String>,
    #[serde(flatten)]
    pub page: PageParams,
}

/// `GET /webhook_deliveries` — the outbound webhook deliveries feed, filterable
/// by endpoint / event / status, paginated newest-first.
pub async fn list_webhook_deliveries(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<WebhookDeliveryIndexQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let rows = st
        .db
        .list_webhook_deliveries(
            org,
            q.webhook_endpoint_id,
            q.event_type.as_deref(),
            q.status.as_deref(),
            q.page.per_page(),
            q.page.offset(),
        )
        .await?;
    let total = st
        .db
        .count_webhook_deliveries(
            org,
            q.webhook_endpoint_id,
            q.event_type.as_deref(),
            q.status.as_deref(),
        )
        .await?;
    Ok(Json(json!({
        "webhook_deliveries": rows.iter().map(delivery_json).collect::<Vec<_>>(),
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

/// `GET /webhook_deliveries/:id` — a single delivery record (org-scoped; a
/// cross-tenant id answers `404`).
pub async fn show_webhook_delivery(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let d = st
        .db
        .find_webhook_delivery(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("webhook_delivery".into()))?;
    Ok(Json(json!({ "webhook_delivery": delivery_json(&d) })))
}

// ---- wallet transactions ---------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct WalletTransactionIndexQuery {
    #[serde(flatten)]
    pub page: PageParams,
}

/// `GET /wallets/:id/transactions` — a wallet's credit/debit ledger, paginated
/// newest-first. The wallet is org-scoped: a wallet owned by another org answers
/// `404` (existence never leaks).
pub async fn wallet_transactions(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(q): Query<WalletTransactionIndexQuery>,
) -> ApiResult<Json<Value>> {
    let wallet = st
        .db
        .find_wallet(id)
        .await?
        .filter(|w| w.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let txns = st
        .db
        .wallet_transactions_for_wallet(wallet.id, q.page.per_page(), q.page.offset())
        .await?;
    let total = st.db.count_wallet_transactions(wallet.id).await?;
    Ok(Json(json!({
        "wallet_transactions": txns.iter().map(ser::wallet_transaction).collect::<Vec<_>>(),
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

// ---- dunning ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DunningIndexQuery {
    #[serde(flatten)]
    pub page: PageParams,
}

/// `GET /dunning` — the overdue-invoice / dunning overview: every finalized,
/// payment-overdue invoice with its customer's campaign + attempt status.
pub async fn list_dunning(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<DunningIndexQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let rows = st
        .db
        .overdue_dunning_overview(org, q.page.per_page(), q.page.offset())
        .await?;
    let total = st.db.count_overdue_invoices(org).await?;
    let collection: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "lago_invoice_id": r.invoice_id,
                "invoice_number": r.invoice_number,
                "external_customer_id": r.external_customer_id,
                "lago_customer_id": r.customer_id,
                "customer_email": r.customer_email,
                "currency": r.currency,
                "total_amount_cents": r.total_amount_cents,
                "payment_due_date": r.payment_due_date.map(|d| d.format("%Y-%m-%d").to_string()),
                "exclude_from_dunning_campaign": r.exclude_from_dunning_campaign,
                "applied_dunning_campaign_id": r.applied_dunning_campaign_id,
                "last_dunning_campaign_attempt_at": r
                    .last_dunning_campaign_attempt_at
                    .map(|t| t.and_utc().to_rfc3339()),
                "dunning_currency_attempts": r.dunning_currency_attempts,
            })
        })
        .collect();
    Ok(Json(json!({
        "dunning": collection,
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

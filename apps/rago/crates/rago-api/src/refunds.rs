//! Refund endpoints — money actually moves at the payment provider.
//!
//! * `POST /invoices/:id/refund` — refund the invoice's captured payment
//!   (optionally `{ "amount_cents": n }` for a partial refund) at the provider
//!   that captured it.
//! * `POST /credit_notes/:id/refund` — (re)issue the provider refund for a
//!   credit note's `refund_amount_cents` (e.g. after a provider failure).
//! * `GET /invoices/:id/refunds` — the refund ledger for an invoice.
//!
//! Credit notes created with a `refund_amount_cents` trigger the provider
//! refund through [`refund_credit_note_at_provider`].

use axum::extract::{Path, State};
use axum::Json;
use rago_core::models::{CreditNote, Invoice};
use rago_jobs::refunds::{RefundRejection, RefundResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
use crate::state::AppState;

fn refund_json(r: &RefundResult) -> Value {
    json!({
        "lago_id": r.refund_id,
        "provider": r.provider,
        "provider_refund_id": r.provider_refund_id,
        "status": r.state.as_str(),
        "amount_cents": r.amount_cents,
        "currency": r.currency,
        "error": r.error,
    })
}

fn rejection(r: RefundRejection) -> ApiError {
    ApiError::validation("refund", &r.code())
}

fn job_error(e: rago_jobs::JobError) -> ApiError {
    match e {
        rago_jobs::JobError::Db(e) => ApiError::from(e),
        other => ApiError::Validation(json!({ "refund": [other.to_string()] })),
    }
}

/// `POST /invoices/:id/refund`.
pub async fn refund_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let amount = body
        .as_ref()
        .and_then(|b| b.get("refund").unwrap_or(&b.0).get("amount_cents").and_then(Value::as_i64));
    let transport = rago_integrations::default_transport();
    let result = rago_jobs::refunds::refund_invoice(&st.db, &transport, &invoice, amount, None)
        .await
        .map_err(job_error)?
        .map_err(rejection)?;
    let updated = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(updated.id).await?;
    Ok(Json(json!({ "invoice": ser::invoice(&updated, &fees), "refund": refund_json(&result) })))
}

/// `GET /invoices/:id/refunds`.
pub async fn list_invoice_refunds(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let rows = st.db.refunds_for_invoice(id).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "lago_id": r.id,
                "lago_payment_id": r.payment_id,
                "lago_credit_note_id": r.credit_note_id,
                "provider": r.provider,
                "provider_refund_id": r.provider_refund_id,
                "status": r.status,
                "amount_cents": r.amount_cents,
                "currency": r.amount_currency,
                "error": r.last_error,
            })
        })
        .collect();
    Ok(Json(json!({ "refunds": out })))
}

/// Issue the provider refund for a credit note's `refund_amount_cents`.
/// Returns `None` when the credit note has nothing to refund.
pub async fn refund_credit_note_at_provider(
    st: &AppState,
    invoice: &Invoice,
    cn: &CreditNote,
) -> Result<Option<Result<RefundResult, RefundRejection>>, rago_jobs::JobError> {
    if cn.refund_amount_cents <= 0 {
        return Ok(None);
    }
    let transport = rago_integrations::default_transport();
    rago_jobs::refunds::refund_invoice(&st.db, &transport, invoice, Some(cn.refund_amount_cents), Some(cn))
        .await
        .map(Some)
}

/// `POST /credit_notes/:id/refund` — issue (or retry after a failure) the
/// provider refund for a credit note.
pub async fn refund_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    if cn.refund_amount_cents <= 0 {
        return Err(ApiError::validation("refund", "nothing_to_refund"));
    }
    if let Some(existing) = st.db.refund_for_credit_note(cn.id).await? {
        if existing.status != "failed" {
            return Err(ApiError::validation("refund", &format!("refund_already_{}", existing.status)));
        }
        // A failed attempt is replaced by the retry (one refund per credit note).
        st.db.delete_failed_refund(existing.id).await?;
    }
    let invoice = st
        .db
        .find_invoice(cn.invoice_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let result = refund_credit_note_at_provider(&st, &invoice, &cn)
        .await
        .map_err(job_error)?
        .ok_or_else(|| ApiError::validation("refund", "nothing_to_refund"))?
        .map_err(rejection)?;
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    Ok(Json(json!({ "credit_note": ser::credit_note(&cn), "refund": refund_json(&result) })))
}

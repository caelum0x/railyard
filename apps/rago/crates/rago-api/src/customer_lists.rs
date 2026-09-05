//! Customer-scoped and org-scoped list endpoints that close the Lago REST tail:
//!
//! * `GET /applied_coupons` (+ `?external_customer_id=`) and
//!   `GET /customers/:external_id/applied_coupons` — `AppliedCouponsQuery`.
//! * `GET /customers/:external_id/credit_notes` — `CreditNotesQuery` (customer).
//! * `GET /customers/:external_id/payments` — `PaymentsQuery` (customer).
//!
//! Each handler authenticates via the api-key extractor, calls the matching
//! [`rago_db::Db`] list op added for this tail, and serializes the Lago-shaped
//! JSON envelope. Coupon / customer lookups per row keep the applied-coupon
//! payload faithful to `V1::AppliedCouponSerializer`.

use axum::extract::{Path, Query, State};
use axum::Json;
use rago_core::models::{AppliedCoupon, Coupon};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
use crate::state::AppState;

// ---- applied coupons -------------------------------------------------------

/// One applied-coupon row in `V1::AppliedCouponSerializer` shape. `coupon` and
/// `external_customer_id` are resolved by the caller so the payload carries the
/// coupon code/name and the customer's external id like Lago.
fn applied_coupon_json(ac: &AppliedCoupon, coupon: Option<&Coupon>, external_customer_id: Option<&str>) -> Value {
    json!({
        "lago_id": ac.id,
        "lago_coupon_id": ac.coupon_id,
        "coupon_code": coupon.map(|c| c.code.clone()),
        "coupon_name": coupon.map(|c| c.name.clone()),
        "lago_customer_id": ac.customer_id,
        "external_customer_id": external_customer_id,
        "status": ac.status.as_str(),
        "amount_cents": ac.amount_cents,
        "amount_currency": ac.amount_currency,
        "percentage_rate": ac.percentage_rate.map(|d| d.to_string()),
        "frequency": ac.frequency.as_str(),
        "frequency_duration": ac.frequency_duration,
        "frequency_duration_remaining": ac.frequency_duration_remaining,
        "created_at": ac.created_at.to_rfc3339(),
        "terminated_at": ac.terminated_at.map(|t| t.to_rfc3339()),
    })
}

/// Serialize a list of applied coupons, resolving each coupon + customer once
/// (caches by id to avoid repeat lookups within the page).
async fn serialize_applied_coupons(
    st: &AppState,
    org: Uuid,
    rows: &[AppliedCoupon],
) -> ApiResult<Vec<Value>> {
    use std::collections::HashMap;
    let mut coupons: HashMap<Uuid, Option<Coupon>> = HashMap::new();
    let mut externals: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for ac in rows {
        if !coupons.contains_key(&ac.coupon_id) {
            coupons.insert(ac.coupon_id, st.db.find_coupon(ac.coupon_id).await?);
        }
        if !externals.contains_key(&ac.customer_id) {
            let ext = st.db.find_customer(ac.customer_id).await?.map(|c| c.external_id);
            externals.insert(ac.customer_id, ext);
        }
        let _ = org;
        let coupon = coupons.get(&ac.coupon_id).and_then(|c| c.as_ref());
        let ext = externals.get(&ac.customer_id).and_then(|e| e.as_deref());
        out.push(applied_coupon_json(ac, coupon, ext));
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct AppliedCouponIndexQuery {
    pub external_customer_id: Option<String>,
}

/// `GET /applied_coupons` — org applied coupons, optionally filtered by
/// `?external_customer_id=` (`AppliedCouponsController#index`).
pub async fn list_applied_coupons(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<AppliedCouponIndexQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let customer_id = match &q.external_customer_id {
        Some(ext) => Some(
            st.db
                .find_customer_by_external_id(org, ext)
                .await?
                .ok_or_else(|| ApiError::NotFound("customer".into()))?
                .id,
        ),
        None => None,
    };
    let rows = st.db.list_applied_coupons(org, customer_id).await?;
    let out = serialize_applied_coupons(&st, org, &rows).await?;
    Ok(Json(json!({ "applied_coupons": out })))
}

/// `GET /customers/:external_id/applied_coupons` — nested index
/// (`Customers::AppliedCouponsController#index`).
pub async fn customer_applied_coupons(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let rows = st.db.list_applied_coupons(org, Some(customer.id)).await?;
    let out = serialize_applied_coupons(&st, org, &rows).await?;
    Ok(Json(json!({ "applied_coupons": out })))
}

// ---- customer credit notes -------------------------------------------------

/// `GET /customers/:external_id/credit_notes` — the customer's credit notes
/// (`Customers::CreditNotesController#index`).
pub async fn customer_credit_notes(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let customer = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let notes = st.db.credit_notes_for_customer(customer.id).await?;
    let out: Vec<Value> = notes.iter().map(ser::credit_note).collect();
    Ok(Json(json!({ "credit_notes": out })))
}

// ---- customer payments -----------------------------------------------------

fn payment_json(r: &rago_db::PaymentRow) -> Value {
    json!({
        "lago_id": r.0,
        "payment_provider_type": r.1,
        "status": r.2,
        "amount_cents": r.3,
        "amount_currency": r.4,
    })
}

/// `GET /customers/:external_id/payments` — payments across the customer's
/// invoices (`Customers::PaymentsController#index`).
pub async fn customer_payments(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let customer = st
        .db
        .find_customer_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let payments = st.db.payments_for_customer(customer.id).await?;
    Ok(Json(json!({ "payments": payments.iter().map(payment_json).collect::<Vec<_>>() })))
}

//! Phase-14 handlers — integration connection OAuth lifecycle, accounting/CRM
//! sync, usage-monitoring alerts, and credit-note / payment-receipt PDFs.
//!
//! All provider network calls go through the build's transport (real HTTP
//! under the `live` feature). Missing provider configuration is reported as a
//! 422 naming the missing setting — never a fabricated provider response.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, s, wrapped};
use crate::state::AppState;

// ---- integration connection OAuth lifecycle --------------------------------
//
// Listing, creating and authorizing connections lives in
// `crate::integration_hub` (per-org, per-provider OAuth apps with a
// single-use state); this module keeps the manual token refresh.

/// `POST /integrations/:id/refresh` — refresh the connection's access token
/// with its stored refresh token against the provider's own token endpoint
/// and the integration's OAuth app (tokens are also refreshed automatically
/// before every sync and by the background maintenance tick).
pub async fn refresh_integration(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let conn = st
        .db
        .find_org_integration(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("integration".into()))?;
    if conn.refresh_token.is_none() {
        return Err(ApiError::validation("refresh_token", "value_is_missing"));
    }
    let transport = rago_integrations::default_transport();
    let tokens = rago_jobs::integration_auth::refresh(&st.db, &transport, &conn)
        .await
        .map_err(|e| crate::integrations::integration_error("oauth", e))?;
    Ok(Json(json!({
        "integration": { "lago_id": id, "refreshed": true, "expires_in": tokens.expires_in }
    })))
}

// ---- accounting sync (Xero / NetSuite) -------------------------------------

/// `POST /invoices/:id/sync_accounting` — sync a finalized invoice to the
/// connected accounting system (Xero or NetSuite) and store the mapping.
pub async fn sync_invoice_accounting(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let transport = rago_integrations::default_transport();
    let external_id =
        rago_jobs::integrations_sync::sync_invoice_accounting(&st.db, &transport, auth.organization.id, id)
            .await
            .map_err(|e| crate::integrations::integration_error("accounting_integration", e))?;
    let provider = st
        .db
        .active_integration_of_type(auth.organization.id, "accounting")
        .await?
        .map(|c| c.provider_code);
    Ok(Json(json!({
        "integration_invoice": {
            "lago_invoice_id": id,
            "provider": provider,
            "external_id": external_id,
        }
    })))
}

// ---- CRM sync (Salesforce / HubSpot) ---------------------------------------

/// `POST /customers/:external_id/sync_crm` — sync a customer to the connected
/// CRM (Salesforce or HubSpot) and store the mapping.
pub async fn sync_customer_crm(
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
    let transport = rago_integrations::default_transport();
    let (provider, crm_id) = rago_jobs::integrations_sync::sync_customer_crm_with_provider(&st.db, &transport, org, &customer)
        .await
        .map_err(|e| crate::integrations::integration_error("crm_integration", e))?;
    Ok(Json(json!({
        "integration_customer": {
            "lago_customer_id": customer.id,
            "external_customer_id": customer.external_id,
            "provider": provider,
            "external_id": crm_id,
        }
    })))
}

// ---- usage-monitoring alerts -----------------------------------------------

/// `POST /subscriptions/:external_id/alerts` — create a usage-monitoring alert.
pub async fn create_usage_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "alert");
    let threshold_cents = i64_of(p, "threshold_cents")
        .ok_or_else(|| ApiError::validation("threshold_cents", "value_is_mandatory"))?;
    let metric_code = s(p, "billable_metric_code");
    // Explicit alert_type (e.g. `lifetime_usage_amount`) overrides the inferred one.
    let alert_type = s(p, "alert_type").unwrap_or_else(|| {
        if metric_code.is_some() {
            "billable_metric_current_usage_amount".into()
        } else {
            "current_usage_amount".into()
        }
    });
    let recurring = p.get("recurring").and_then(Value::as_bool).unwrap_or(false);
    let id = Uuid::new_v4();
    st.db
        .insert_usage_alert(id, org, &external_id, &alert_type, metric_code.as_deref(), s(p, "code").as_deref(), threshold_cents, recurring)
        .await?;

    Ok(Json(json!({
        "alert": {
            "lago_id": id,
            "external_subscription_id": external_id,
            "alert_type": alert_type,
            "billable_metric_code": metric_code,
            "threshold_cents": threshold_cents,
            "recurring": recurring,
        }
    })))
}

/// `GET /subscriptions/:external_id/alerts` — alerts + their trigger history.
pub async fn list_usage_alerts(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let alerts = st.db.list_usage_alerts(org, &external_id).await?;
    let triggers = st.db.list_alert_triggers(org, &external_id).await?;
    let alerts_json: Vec<Value> = alerts.into_iter().map(|(id, atype, metric, threshold, recurring, prev, last)| json!({
        "lago_id": id, "alert_type": atype, "billable_metric_code": metric,
        "threshold_cents": threshold, "recurring": recurring, "previous_value_cents": prev,
        "last_triggered_at": last.map(|t| t.and_utc().to_rfc3339()),
    })).collect();
    let triggers_json: Vec<Value> = triggers.into_iter().map(|(alert_id, crossed, current, at)| json!({
        "usage_alert_id": alert_id, "crossed_threshold_cents": crossed,
        "current_value_cents": current, "triggered_at": at.and_utc().to_rfc3339(),
    })).collect();
    Ok(Json(json!({ "alerts": alerts_json, "triggers": triggers_json })))
}

/// Evaluate a subscription's usage alerts against the current usage amount and
/// per-metric amounts. Fires (persists a trigger + an outbound `alert.triggered`
/// webhook) for each crossed threshold. Returns the number fired.
///
/// Called from the current-usage read path so alerts evaluate against live
/// usage (`UsageMonitoring::ProcessSubscriptionActivityService`).
pub async fn evaluate_usage_alerts(
    st: &AppState,
    org: Uuid,
    sub_ext: &str,
    total_amount_cents: i64,
    per_metric: &std::collections::HashMap<String, i64>,
) -> ApiResult<u32> {
    let alerts = st.db.list_usage_alerts(org, sub_ext).await?;
    let mut fired = 0u32;
    for (id, alert_type, metric_code, threshold, recurring, previous_value, last_triggered) in alerts {
        let current = match &metric_code {
            Some(code) => per_metric.get(code).copied().unwrap_or(0),
            None => total_amount_cents,
        };
        let crossed = if recurring {
            threshold > 0 && (current / threshold) > (previous_value / threshold)
        } else {
            current >= threshold && last_triggered.is_none()
        };
        if !crossed {
            continue;
        }
        st.db.insert_alert_trigger(Uuid::new_v4(), org, id, sub_ext, threshold, current).await?;
        st.db.mark_alert_triggered(id, current).await?;

        // Fire an outbound `alert.triggered` webhook (durably queued via the
        // catalog-keyed path so the envelope carries the correct
        // `webhook_type`/`object_type` = "alert.triggered"/"triggered_alert").
        crate::emit::webhook(
            &st.db,
            org,
            rago_integrations::webhooks::WebhookKind::AlertTriggered,
            json!({
                "lago_id": id,
                "external_subscription_id": sub_ext,
                "alert_type": alert_type,
                "billable_metric_code": metric_code,
                "threshold_cents": threshold,
                "current_value_cents": current,
            }),
        )
        .await;
        crate::observability::incr("usage_alerts_fired");
        crate::observability::incr("webhooks_enqueued");
        fired += 1;
    }
    Ok(fired)
}

// ---- credit-note + payment-receipt PDFs ------------------------------------

/// `GET /credit_notes/:id/download` — the credit-note PDF.
pub async fn download_credit_note_pdf(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    let invoice = st.db.find_invoice(cn.invoice_id).await?;
    let invoice_number = invoice.as_ref().map(|i| i.number.clone()).unwrap_or_default();
    let ext = st.db.customer_external_id(cn.customer_id).await?.unwrap_or_default();
    let pdf = rago_documents::credit_note_pdf(&cn, &invoice_number, &ext);
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}.pdf\"", cn.number),
        ),
    ];
    Ok((headers, pdf).into_response())
}

/// `GET /invoices/:id/receipt` — the payment-receipt PDF.
pub async fn download_payment_receipt_pdf(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let (name, ext) = match invoice.customer_id {
        Some(cid) => {
            let c = st.db.find_customer(cid).await?;
            let ext = c.as_ref().map(|c| c.external_id.clone()).unwrap_or_default();
            let name = c.and_then(|c| c.name).unwrap_or_default();
            (name, ext)
        }
        None => (String::new(), String::new()),
    };
    let pdf = rago_documents::payment_receipt_pdf(&invoice, &name, &ext);
    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"receipt-{}.pdf\"", invoice.number),
        ),
    ];
    Ok((headers, pdf).into_response())
}

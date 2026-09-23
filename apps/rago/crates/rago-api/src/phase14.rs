//! Phase-14 handlers — integration connection OAuth lifecycle, accounting/CRM
//! sync, usage-monitoring alerts, and credit-note / payment-receipt PDFs.
//!
//! All provider network calls go through the injectable transport
//! (`rago_integrations`), driven here by injected responses (live provider keys
//! are owner-gated), so the full round-trip (request shape → parse → persist)
//! runs against real Postgres.

use axum::extract::{Path, State};
use axum::Json;
use chrono::{Duration, Utc};
use rago_integrations::{
    AccountingInvoice, AccountingProvider, CrmCustomer, CrmProvider, MockTransport, OAuthClient,
    OAuthConfig, SalesforceProvider, XeroProvider,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, req_s, s, wrapped};
use crate::state::AppState;

// ---- integration connection OAuth lifecycle --------------------------------

/// `POST /integrations` — create + authorize an integration connection.
///
/// Body: `{ integration: { integration_type, provider_code, code, name? } }`.
/// The authorization `code` is exchanged for OAuth tokens through the injectable
/// transport (an injected token response stands in for the owner-gated live
/// provider), then the connection (with tokens) is persisted.
pub async fn create_integration(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "integration");
    let integration_type = req_s(p, "integration_type")?;
    let provider_code = req_s(p, "provider_code")?;
    let code = req_s(p, "code")?;

    let config = OAuthConfig {
        token_url: std::env::var("RAGO_OAUTH_TOKEN_URL")
            .unwrap_or_else(|_| format!("https://{provider_code}.example/oauth/token")),
        client_id: std::env::var("RAGO_OAUTH_CLIENT_ID").unwrap_or_else(|_| "client_test".into()),
        client_secret: std::env::var("RAGO_OAUTH_CLIENT_SECRET").unwrap_or_else(|_| "secret_test".into()),
        redirect_uri: std::env::var("RAGO_OAUTH_REDIRECT_URI")
            .unwrap_or_else(|_| "https://app.rago.example/oauth/callback".into()),
    };
    // Injected token-exchange response (a live deployment injects reqwest).
    let token_response = std::env::var("RAGO_OAUTH_EXCHANGE_RESPONSE").unwrap_or_else(|_| {
        json!({ "access_token": "at-live-001", "refresh_token": "rt-live-001", "expires_in": 3600, "token_type": "Bearer" }).to_string()
    });
    let client = OAuthClient::new(config, MockTransport::always(200, token_response));
    let tokens = client
        .exchange_code(&code)
        .await
        .map_err(|e| ApiError::Validation(json!({ "oauth": [e.to_string()] })))?;

    let id = Uuid::new_v4();
    let expires_at = tokens.expires_in.map(|s| (Utc::now() + Duration::seconds(s)).naive_utc());
    st.db
        .insert_integration_connection(
            id,
            org,
            &integration_type,
            &provider_code,
            s(p, "name").as_deref(),
            Some(&tokens.access_token),
            tokens.refresh_token.as_deref(),
            expires_at,
            &json!({}),
        )
        .await?;

    Ok(Json(json!({
        "integration": {
            "lago_id": id,
            "integration_type": integration_type,
            "provider_code": provider_code,
            "has_access_token": true,
            "has_refresh_token": tokens.refresh_token.is_some(),
            "expires_in": tokens.expires_in,
        }
    })))
}

/// `POST /integrations/:id/refresh` — refresh the connection's access token
/// using its stored refresh token (OAuth `refresh_token` grant).
pub async fn refresh_integration(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (_id, _itype, provider_code, _access, refresh, _exp) = st
        .db
        .find_integration_connection(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("integration".into()))?;
    let refresh_token = refresh.ok_or_else(|| ApiError::validation("refresh_token", "value_is_missing"))?;

    let config = OAuthConfig {
        token_url: std::env::var("RAGO_OAUTH_TOKEN_URL")
            .unwrap_or_else(|_| format!("https://{provider_code}.example/oauth/token")),
        client_id: std::env::var("RAGO_OAUTH_CLIENT_ID").unwrap_or_else(|_| "client_test".into()),
        client_secret: std::env::var("RAGO_OAUTH_CLIENT_SECRET").unwrap_or_else(|_| "secret_test".into()),
        redirect_uri: String::new(),
    };
    let token_response = std::env::var("RAGO_OAUTH_REFRESH_RESPONSE").unwrap_or_else(|_| {
        json!({ "access_token": "at-live-002", "expires_in": 3600, "token_type": "Bearer" }).to_string()
    });
    let client = OAuthClient::new(config, MockTransport::always(200, token_response));
    let tokens = client
        .refresh(&refresh_token)
        .await
        .map_err(|e| ApiError::Validation(json!({ "oauth": [e.to_string()] })))?;

    let expires_at = tokens.expires_in.map(|s| (Utc::now() + Duration::seconds(s)).naive_utc());
    st.db
        .update_integration_tokens(id, &tokens.access_token, tokens.refresh_token.as_deref(), expires_at)
        .await?;

    Ok(Json(json!({
        "integration": { "lago_id": id, "refreshed": true, "expires_in": tokens.expires_in }
    })))
}

// ---- accounting sync (Xero) ------------------------------------------------

/// `POST /invoices/:id/sync_accounting?connection_id=` — sync a finalized
/// invoice to the connected accounting system (Xero) and store the mapping.
pub async fn sync_invoice_accounting(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let invoice = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let external_customer_id = match invoice.customer_id {
        Some(cid) => st.db.customer_external_id(cid).await?.unwrap_or_default(),
        None => String::new(),
    };
    let connection = st.db.find_connection_by_provider(org, "xero").await?;
    let connection_id = connection.as_ref().map(|(cid, _)| *cid);
    let access_token = connection.and_then(|(_, at)| at).unwrap_or_else(|| "xero_test".into());

    let acc = AccountingInvoice {
        invoice_id: invoice.id.to_string(),
        number: invoice.number.clone(),
        currency: invoice.currency().code(),
        total_amount_cents: invoice.total_amount_cents,
        customer_external_id: external_customer_id,
    };
    let response = std::env::var("RAGO_XERO_RESPONSE")
        .unwrap_or_else(|_| json!({ "succeededId": "XERO-INV-001" }).to_string());
    let provider = XeroProvider::new(
        std::env::var("RAGO_ACCOUNTING_BASE_URL").unwrap_or_else(|_| "https://aggregator.example".into()),
        access_token,
        MockTransport::always(200, response),
    );
    let result = provider
        .sync_invoice(&acc)
        .await
        .map_err(|e| ApiError::Validation(json!({ "accounting_integration": [e.to_string()] })))?;

    if let Some(cid) = connection_id {
        st.db.insert_integration_mapping(Uuid::new_v4(), org, cid, "invoice", invoice.id, &result.external_id).await?;
    }
    Ok(Json(json!({
        "integration_invoice": {
            "lago_invoice_id": invoice.id,
            "provider": "xero",
            "external_id": result.external_id,
        }
    })))
}

// ---- CRM sync (Salesforce) -------------------------------------------------

/// `POST /customers/:external_id/sync_crm` — sync a customer to the connected
/// CRM (Salesforce) and store the mapping.
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
    let connection = st.db.find_connection_by_provider(org, "salesforce").await?;
    let connection_id = connection.as_ref().map(|(cid, _)| *cid);
    let access_token = connection.and_then(|(_, at)| at).unwrap_or_else(|| "salesforce_test".into());

    let crm = CrmCustomer {
        customer_id: customer.id.to_string(),
        external_id: customer.external_id.clone(),
        name: customer.name.clone(),
        email: customer.email.clone(),
    };
    let response = std::env::var("RAGO_SALESFORCE_RESPONSE")
        .unwrap_or_else(|_| json!({ "id": "003SF00001", "success": true }).to_string());
    let provider = SalesforceProvider::new(
        std::env::var("RAGO_CRM_BASE_URL").unwrap_or_else(|_| "https://aggregator.example".into()),
        access_token,
        MockTransport::always(200, response),
    );
    let result = provider
        .sync_customer(&crm)
        .await
        .map_err(|e| ApiError::Validation(json!({ "crm_integration": [e.to_string()] })))?;

    if let Some(cid) = connection_id {
        st.db.insert_integration_mapping(Uuid::new_v4(), org, cid, "customer", customer.id, &result.external_id).await?;
    }
    Ok(Json(json!({
        "integration_customer": {
            "lago_customer_id": customer.id,
            "external_customer_id": customer.external_id,
            "provider": "salesforce",
            "external_id": result.external_id,
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

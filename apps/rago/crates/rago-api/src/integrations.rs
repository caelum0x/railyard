//! Tax-integration endpoint — fetch external tax rates for an invoice and
//! apply them to its fees (`Integrations::Aggregator::Taxes` +
//! `Fees::ApplyProviderTaxesService` + invoice tax rollup).
//!
//! The provider call goes through the build's transport (real HTTP under the
//! `live` feature) to the configured aggregator. Missing configuration is a 422
//! naming what is missing — never a fabricated tax response.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct TaxProviderQuery {
    /// `anrok` (default) or `avalara`.
    pub provider: Option<String>,
}

/// `POST /invoices/:id/apply_provider_taxes?provider=` — fetch + apply external
/// taxes through the selected tax provider (Anrok or Avalara) via the
/// configured aggregator (`RAGO_TAX_BASE_URL` / `RAGO_TAX_API_KEY`), then
/// persist the fee taxes and invoice totals.
pub async fn apply_provider_taxes_to_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(pq): Query<TaxProviderQuery>,
) -> ApiResult<Json<Value>> {
    let provider_code = pq.provider.as_deref().unwrap_or("anrok");
    let transport = rago_integrations::default_transport();
    let (invoice, fees) = rago_jobs::integrations_sync::apply_provider_taxes_to_invoice(
        &st.db,
        &transport,
        auth.organization.id,
        id,
        provider_code,
    )
    .await
    .map_err(|e| integration_error("tax_integration", e))?;
    Ok(Json(json!({ "invoice": ser::invoice(&invoice, &fees) })))
}

/// Map an integration job error to the REST envelope: missing configuration
/// and provider failures are 422 validation errors naming the integration.
pub(crate) fn integration_error(field: &str, e: rago_jobs::JobError) -> ApiError {
    match e {
        rago_jobs::JobError::Db(db) => ApiError::from(db),
        rago_jobs::JobError::NotConfigured(what) => {
            ApiError::Validation(json!({ field: [format!("not_configured: {what}")] }))
        }
        rago_jobs::JobError::Integration(msg) if msg.ends_with("_not_found") => {
            ApiError::NotFound(msg.trim_end_matches("_not_found").to_string())
        }
        other => ApiError::Validation(json!({ field: [other.to_string()] })),
    }
}

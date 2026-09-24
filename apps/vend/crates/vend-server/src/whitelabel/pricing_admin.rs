//! Per-model markup: the founder's price list on top of provider cost.
//!
//! - `GET    …/apps/:app_id/model-pricing` — every priced model the app may
//!   call: provider rates, the app default markup, the override (if any) and
//!   the effective end-user rates;
//! - `PUT    …/apps/:app_id/model-pricing` `{model, markup_bps, min_fee_micros?}`;
//! - `DELETE …/apps/:app_id/model-pricing?model=…` — back to the app default.
//!
//! Served under `/v1/customer` (owner/admin session; viewers may read) and
//! `/v1/control` (operator token). The data plane reads overrides with the
//! app on every request, so a change applies to the next call.

use axum::extract::{Extension, Path, Query, State};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use vend_core::error::VendError;
use vend_core::ids::AppId;
use vend_core::money::ModelMarkup;
use vend_store::{AuditActorKind, AuditOutcome, CustomerPrincipal, CustomerRole};
use vend_tenancy::App;

use super::wallets::owned_app;
use super::{audit, fail, ok, require_role, ConsoleApiState};

#[derive(Deserialize)]
pub struct SetMarkupRequest {
    pub model: String,
    pub markup_bps: u32,
    #[serde(default)]
    pub min_fee_micros: Option<i64>,
}

#[derive(Deserialize)]
pub struct ModelQuery {
    pub model: String,
}

fn marked_up(micros: i64, markup_bps: u32) -> i64 {
    let scaled = i128::from(micros) * (10_000 + i128::from(markup_bps)) / 10_000;
    i64::try_from(scaled).unwrap_or(i64::MAX)
}

/// The price sheet for one app.
fn price_sheet(state: &ConsoleApiState, app: &App) -> Value {
    let models: Vec<Value> = state
        .pricing
        .models()
        .into_iter()
        .filter(|(model, _)| app.allows_model(model))
        .map(|(model, rate)| {
            let effective = app.policy_for(&model);
            json!({
                "model": model,
                "provider_input_micros_per_million_tokens": rate.input_micros_per_million_tokens,
                "provider_output_micros_per_million_tokens": rate.output_micros_per_million_tokens,
                "override": app.model_markups.get(&model),
                "effective_markup_bps": effective.markup_bps,
                "effective_min_fee_micros": effective.min_fee_micros,
                "input_micros_per_million_tokens":
                    marked_up(rate.input_micros_per_million_tokens, effective.markup_bps),
                "output_micros_per_million_tokens":
                    marked_up(rate.output_micros_per_million_tokens, effective.markup_bps),
            })
        })
        .collect();
    json!({
        "app_id": app.id.as_str(),
        "default": app.markup_policy,
        "models": models,
    })
}

async fn load_app(state: &ConsoleApiState, raw: &str) -> Result<App, VendError> {
    let id = AppId::parse(raw)?;
    state
        .tenants
        .load_app(&id)
        .await?
        .ok_or(VendError::NotFound)
}

async fn apply_set(
    state: &ConsoleApiState,
    app: &App,
    input: &SetMarkupRequest,
) -> Result<Value, VendError> {
    let model = input.model.trim();
    // Only priced models the app may call can carry an override; anything else
    // could never be billed and would only confuse the price list.
    let priced = state.pricing.models().into_iter().any(|(m, _)| m == model);
    if !priced || !app.allows_model(model) {
        return Err(VendError::ModelNotAllowed);
    }
    let markup = ModelMarkup {
        markup_bps: input.markup_bps,
        min_fee_micros: input.min_fee_micros,
    };
    markup.validate()?;
    state
        .whitelabel
        .set_model_markup(&app.id, model, markup)
        .await?;
    let refreshed = load_app(state, app.id.as_str()).await?;
    Ok(price_sheet(state, &refreshed))
}

async fn apply_delete(state: &ConsoleApiState, app: &App, model: &str) -> Result<Value, VendError> {
    if !state.whitelabel.delete_model_markup(&app.id, model).await? {
        return Err(VendError::NotFound);
    }
    let refreshed = load_app(state, app.id.as_str()).await?;
    Ok(price_sheet(state, &refreshed))
}

async fn audit_change(
    state: &ConsoleApiState,
    app: &App,
    actor: (AuditActorKind, Option<String>),
    action: &str,
    model: &str,
    ok: bool,
) {
    audit(
        &state.security_repo,
        Some(app.tenant_id.clone()),
        actor,
        action,
        ("app", app.id.as_str()),
        if ok {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        },
        json!({"model": model}),
    )
    .await;
}

// --- customer --------------------------------------------------------------------

pub async fn customer_get(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
) -> Response {
    match owned_app(&state, &principal, &raw).await {
        Ok(app) => ok(price_sheet(&state, &app)),
        Err(error) => fail(error),
    }
}

pub async fn customer_put(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<SetMarkupRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let result = apply_set(&state, &app, &input).await;
    let actor = (AuditActorKind::Customer, Some(principal.user_id.clone()));
    audit_change(
        &state,
        &app,
        actor,
        "pricing.model_markup_set",
        &input.model,
        result.is_ok(),
    )
    .await;
    match result {
        Ok(sheet) => ok(sheet),
        Err(error) => fail(error),
    }
}

pub async fn customer_delete(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Query(query): Query<ModelQuery>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let result = apply_delete(&state, &app, &query.model).await;
    let actor = (AuditActorKind::Customer, Some(principal.user_id.clone()));
    audit_change(
        &state,
        &app,
        actor,
        "pricing.model_markup_cleared",
        &query.model,
        result.is_ok(),
    )
    .await;
    match result {
        Ok(sheet) => ok(sheet),
        Err(error) => fail(error),
    }
}

// --- operator ----------------------------------------------------------------------

pub async fn operator_get(
    State(state): State<ConsoleApiState>,
    Path(raw): Path<String>,
) -> Response {
    match load_app(&state, &raw).await {
        Ok(app) => ok(price_sheet(&state, &app)),
        Err(error) => fail(error),
    }
}

pub async fn operator_put(
    State(state): State<ConsoleApiState>,
    Path(raw): Path<String>,
    Json(input): Json<SetMarkupRequest>,
) -> Response {
    let app = match load_app(&state, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let result = apply_set(&state, &app, &input).await;
    audit_change(
        &state,
        &app,
        (AuditActorKind::Administrator, None),
        "pricing.model_markup_set",
        &input.model,
        result.is_ok(),
    )
    .await;
    match result {
        Ok(sheet) => ok(sheet),
        Err(error) => fail(error),
    }
}

pub async fn operator_delete(
    State(state): State<ConsoleApiState>,
    Path(raw): Path<String>,
    Query(query): Query<ModelQuery>,
) -> Response {
    let app = match load_app(&state, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let result = apply_delete(&state, &app, &query.model).await;
    audit_change(
        &state,
        &app,
        (AuditActorKind::Administrator, None),
        "pricing.model_markup_cleared",
        &query.model,
        result.is_ok(),
    )
    .await;
    match result {
        Ok(sheet) => ok(sheet),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::marked_up;

    #[test]
    fn marked_up_rates_apply_basis_points() {
        assert_eq!(marked_up(1_000_000, 2_500), 1_250_000);
        assert_eq!(marked_up(1_000_000, 0), 1_000_000);
        assert_eq!(marked_up(i64::MAX, 1_000_000), i64::MAX);
    }
}

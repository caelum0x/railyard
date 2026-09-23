//! `GET /v1/relay/provider_health`: routing and adaptive provider health.
//!
//! Joins the configured routing of every chat model (provider order, hedging,
//! retries) with the live in-process health scorer that drives adaptive
//! ranking and circuit breaking, so operators can see why a provider was
//! skipped and whether a model has any failover path at all.

use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use relay_hedge::{CircuitSnapshot, ProviderHealthSnapshot};
use serde::Serialize;
use tensorzero_core::utils::gateway::{AppState, AppStateData};

/// Health of one `(model, provider)` route.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRouteStatus {
    pub provider: String,
    /// Position in the model's `routing` list (0 = primary).
    pub priority: usize,
    /// `closed` (healthy), `open` (skipped until cooldown), or `half_open`.
    pub circuit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_cooldown_ms: Option<u64>,
    pub ewma_latency_ms: f64,
    pub ewma_ttft_ms: f64,
    pub error_rate: f64,
    pub total_attempts: u64,
    pub failed_attempts: u64,
    /// `false` until the gateway has observed traffic on this route.
    pub observed: bool,
}

/// Routing summary for one configured model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelRoutingStatus {
    pub model: String,
    pub hedging_enabled: bool,
    pub hedge_max_concurrency: usize,
    /// `true` when the model has more than one provider to fail over to.
    pub has_fallback: bool,
    pub providers: Vec<ProviderRouteStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthReport {
    pub generated_at_ms: i64,
    pub models: Vec<ModelRoutingStatus>,
    /// Health records for routes no longer in the live config.
    pub unrouted: Vec<ProviderRouteStatus>,
}

pub async fn provider_health_handler(State(AppStateData { config, .. }): AppState) -> Response {
    let snapshots = tensorzero_core::model::provider_health_snapshot();
    let mut models = config
        .models
        .table
        .iter()
        .map(|(name, model)| {
            let providers = model
                .routing
                .iter()
                .enumerate()
                .map(|(priority, provider)| {
                    let snapshot = snapshots
                        .iter()
                        .find(|s| s.model_name == **name && s.provider_name == **provider);
                    route_status(provider.to_string(), priority, snapshot)
                })
                .collect::<Vec<_>>();
            ModelRoutingStatus {
                model: name.to_string(),
                hedging_enabled: model.hedge.enabled,
                hedge_max_concurrency: model.hedge.max_concurrency.max(1),
                has_fallback: model.routing.len() > 1,
                providers,
            }
        })
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.model.cmp(&b.model));
    let unrouted = snapshots
        .iter()
        .filter(|s| {
            config
                .models
                .table
                .get(s.model_name.as_str())
                .is_none_or(|model| !model.routing.iter().any(|p| **p == *s.provider_name))
        })
        .map(|s| route_status(format!("{}/{}", s.model_name, s.provider_name), 0, Some(s)))
        .collect();
    Json(relay_contract::DataEnvelope {
        data: ProviderHealthReport {
            generated_at_ms: relay_billing::now_ms(),
            models,
            unrouted,
        },
    })
    .into_response()
}

fn route_status(
    provider: String,
    priority: usize,
    snapshot: Option<&ProviderHealthSnapshot>,
) -> ProviderRouteStatus {
    let Some(snapshot) = snapshot else {
        return ProviderRouteStatus {
            provider,
            priority,
            circuit: "closed",
            circuit_cooldown_ms: None,
            ewma_latency_ms: 0.0,
            ewma_ttft_ms: 0.0,
            error_rate: 0.0,
            total_attempts: 0,
            failed_attempts: 0,
            observed: false,
        };
    };
    let score = &snapshot.score;
    let (circuit, circuit_cooldown_ms) = match score.circuit {
        CircuitSnapshot::Closed => ("closed", None),
        CircuitSnapshot::Open {
            remaining_cooldown_ms,
        } => ("open", Some(remaining_cooldown_ms)),
        CircuitSnapshot::HalfOpen => ("half_open", None),
    };
    ProviderRouteStatus {
        provider,
        priority,
        circuit,
        circuit_cooldown_ms,
        ewma_latency_ms: score.ewma_latency_ms,
        ewma_ttft_ms: score.ewma_ttft_ms,
        error_rate: score.error_rate,
        total_attempts: score.total,
        failed_attempts: score.failures,
        observed: true,
    }
}

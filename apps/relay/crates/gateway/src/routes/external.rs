//! External route definitions for the TensorZero Gateway API.
//!
//! This file should remain minimal, containing only endpoint path definitions and their handler mappings.
//! Router construction logic belongs in `router.rs`. This constraint exists because CODEOWNERS
//! requires specific review for route changes.
// All functions here are router builders parameterized on `SwappableAppStateData` by axum's type system.
#![expect(
    clippy::disallowed_types,
    reason = "router builders are parameterized on SwappableAppStateData by axum's type system"
)]

use axum::{
    Router,
    routing::{delete, get, patch, post, put},
};
use metrics_exporter_prometheus::PrometheusHandle;
use tensorzero_core::endpoints::openai_compatible::build_openai_compatible_routes;
use tensorzero_core::observability::OtelEnabledRoutes;
use tensorzero_core::{endpoints, utils::gateway::SwappableAppStateData};

/// Defines routes that should have top-level OpenTelemetry HTTP spans created
/// All of these routes will have a span named `METHOD <ROUTE>` (e.g. `POST /batch_inference/{batch_id}`)
/// sent to OpenTelemetry
pub fn build_otel_enabled_routes() -> (OtelEnabledRoutes, Router<SwappableAppStateData>) {
    let mut routes = vec![
        ("/inference", post(endpoints::inference::inference_handler)),
        // Passport-conditioned generation surface. The Context Passport submits a
        // resolved, provider-conditioned generation here; the payload is a native
        // TensorZero inference request (function `passport_<modality>` + the
        // assembled conditioning as input) so it reuses the exact inference
        // pipeline and the success-only billing/AITP settlement middleware that
        // wraps `/inference` (see `billing::is_billable_path`). The response
        // carries the standard `usage` object plus the `AITP-Receipt` /
        // `AITP-Request-ID` headers the passport client turns into signed payment
        // evidence.
        (
            "/v1/passport/generate",
            post(endpoints::inference::inference_handler),
        ),
        (
            "/batch_inference",
            post(endpoints::batch_inference::start_batch_inference_handler),
        ),
        (
            "/batch_inference/{batch_id}",
            get(endpoints::batch_inference::poll_batch_inference_handler),
        ),
        (
            "/batch_inference/{batch_id}/inference/{inference_id}",
            get(endpoints::batch_inference::poll_batch_inference_handler),
        ),
        ("/feedback", post(endpoints::feedback::feedback_handler)),
    ];
    routes.extend(build_openai_compatible_routes().routes);
    let mut router = Router::new();
    let mut route_names = Vec::with_capacity(routes.len());
    for (path, handler) in routes {
        route_names.push(path);
        router = router.route(path, handler);
    }
    (
        OtelEnabledRoutes {
            routes: route_names,
        },
        router,
    )
}

/// Builds external routes that don't have OpenTelemetry tracing.
pub fn build_non_otel_enabled_routes(
    metrics_handle: PrometheusHandle,
) -> Router<SwappableAppStateData> {
    Router::new()
        .merge(build_observability_routes())
        .merge(build_datasets_routes())
        .merge(build_optimization_routes())
        .merge(build_gepa_routes())
        .merge(build_evaluations_routes())
        .merge(build_meta_observability_routes(metrics_handle))
}

/// This function builds the public routes for observability.
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_observability_routes() -> Router<SwappableAppStateData> {
    Router::new()
        .route(
            "/v1/inferences/list_inferences",
            post(endpoints::stored_inferences::v1::list_inferences_handler),
        )
        .route(
            "/v1/inferences/get_inferences",
            post(endpoints::stored_inferences::v1::get_inferences_handler),
        )
}

/// This function builds the public routes for datasets.
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_datasets_routes() -> Router<SwappableAppStateData> {
    Router::new()
        .route(
            "/v1/datasets/{dataset_name}/datapoints",
            post(endpoints::datasets::v1::create_datapoints_handler)
                .patch(endpoints::datasets::v1::update_datapoints_handler)
                .delete(endpoints::datasets::v1::delete_datapoints_handler),
        )
        .route(
            "/v1/datasets/{dataset_name}/datapoints/metadata",
            patch(endpoints::datasets::v1::update_datapoints_metadata_handler),
        )
        .route(
            "/v1/datasets/{dataset_name}/from_inferences",
            post(endpoints::datasets::v1::create_from_inferences_handler),
        )
        .route(
            "/v1/datasets/{dataset_name}/list_datapoints",
            post(endpoints::datasets::v1::list_datapoints_handler),
        )
        .route(
            "/v1/datasets/{dataset_name}",
            delete(endpoints::datasets::v1::delete_dataset_handler),
        )
        .route(
            "/v1/datasets/{dataset_name}/get_datapoints",
            post(endpoints::datasets::v1::get_datapoints_by_dataset_handler),
        )
        // DEPRECATED: prefer /v1/datasets/{dataset_name}/get_datapoints
        .route(
            "/v1/datasets/get_datapoints",
            post(endpoints::datasets::v1::get_datapoints_handler),
        )
}

/// This function builds the public routes for optimization.
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_optimization_routes() -> Router<SwappableAppStateData> {
    Router::new()
        .route(
            "/experimental_optimization_workflow",
            post(tensorzero_optimizers::endpoints::launch_optimization_workflow_handler),
        )
        .route(
            "/experimental_optimization/{job_handle}",
            get(tensorzero_optimizers::endpoints::poll_optimization_handler),
        )
}

/// This function builds the public routes for GEPA optimization.
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_gepa_routes() -> Router<SwappableAppStateData> {
    Router::new()
        .route(
            "/v1/optimization/gepa",
            post(tensorzero_optimizers::endpoints::gepa_launch_handler),
        )
        .route(
            "/v1/optimization/gepa/{task_id}",
            get(tensorzero_optimizers::endpoints::gepa_get_handler),
        )
}

/// This function builds the public routes for evaluations.
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_evaluations_routes() -> Router<SwappableAppStateData> {
    Router::new()
        // Workflow evaluation endpoints (new)
        .route(
            "/workflow_evaluation_run",
            post(endpoints::workflow_evaluation_run::workflow_evaluation_run_handler),
        )
        .route(
            "/workflow_evaluation_run/{run_id}/episode",
            post(endpoints::workflow_evaluation_run::workflow_evaluation_run_episode_handler),
        )
}

/// This function builds the public routes for meta-observability (e.g. gateway health).
///
/// IMPORTANT: Add internal routes to `internal.rs` instead.
fn build_meta_observability_routes(
    metrics_handle: PrometheusHandle,
) -> Router<SwappableAppStateData> {
    Router::new()
        .route(
            "/metrics",
            get(move || std::future::ready(metrics_handle.render())),
        )
        .route("/status", get(endpoints::status::status_handler))
        .route("/health", get(crate::relay_health::health_handler))
        .route("/v1/relay/catalog", get(crate::catalog::catalog_handler))
        .route("/v1/relay/quotes", post(crate::catalog::quote_handler))
        .route(
            "/relay/billing/summary",
            get(crate::billing_summary::billing_summary_handler),
        )
        .route(
            "/relay/billing/settlements",
            get(crate::billing_summary::billing_settlements_handler),
        )
        .route(
            "/relay/billing/keys",
            get(crate::billing_summary::billing_keys_handler),
        )
        .route(
            "/relay/billing/status",
            get(crate::relay_access::billing_status_handler),
        )
        .route(
            "/relay/billing/wallet",
            get(crate::relay_access::wallet_balance_handler),
        )
        .route(
            "/relay/billing/budgets",
            get(crate::billing_summary::list_budgets_handler),
        )
        .route(
            "/relay/billing/budgets/{key_id}",
            put(crate::billing_summary::upsert_budget_handler)
                .delete(crate::billing_summary::delete_budget_handler),
        )
        .route(
            "/v1/relay/provider_health",
            get(crate::provider_status::provider_health_handler),
        )
        .route(
            "/v1/billing/cost-events",
            get(crate::cost_feed::list_cost_events_handler),
        )
        .route(
            "/v1/billing/cost-events/{event_id}",
            get(crate::cost_feed::inspect_cost_event_handler),
        )
        .route(
            "/v1/billing/cost-events/{event_id}/replay",
            post(crate::cost_feed::replay_cost_event_handler),
        )
}

//! Relay-aware readiness layered over TensorZero's dependency probes.

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::join;
use relay_billing::BillingLedger;
use relay_catalog::CatalogService;
use relay_contract::GatewayHealth;
use tensorzero_core::{
    db::HealthCheckable,
    utils::gateway::{AppState, AppStateData},
};

#[derive(Clone)]
pub struct RelayHealthState {
    catalog: Arc<CatalogService>,
    billing: Option<Arc<dyn BillingLedger>>,
}

impl RelayHealthState {
    pub fn new(catalog: Arc<CatalogService>) -> Self {
        Self {
            catalog,
            billing: None,
        }
    }

    #[must_use]
    pub fn with_billing(mut self, billing: Arc<dyn BillingLedger>) -> Self {
        self.billing = Some(billing);
        self
    }

    async fn catalog_is_healthy(&self) -> bool {
        self.catalog.snapshot().await.validate().is_ok()
    }

    async fn billing_status(&self) -> &'static str {
        match &self.billing {
            Some(billing) if billing.health().await.is_ok() => "ok",
            Some(_) => "error",
            None => "disabled",
        }
    }
}

pub async fn health_handler(
    State(AppStateData {
        clickhouse_connection_info,
        postgres_connection_info,
        valkey_connection_info,
        valkey_cache_connection_info,
        ..
    }): AppState,
    Extension(relay): Extension<RelayHealthState>,
) -> Response {
    let (clickhouse, postgres, valkey, valkey_cache, catalog, billing) = join!(
        clickhouse_connection_info.health(),
        postgres_connection_info.health(),
        valkey_connection_info.health(),
        valkey_cache_connection_info.health(),
        relay.catalog_is_healthy(),
        relay.billing_status(),
    );
    let health = GatewayHealth {
        gateway: "ok".into(),
        clickhouse: component(clickhouse.is_ok()),
        postgres: component(postgres.is_ok()),
        valkey: component(valkey.is_ok()),
        valkey_cache: component(valkey_cache.is_ok()),
        relay_catalog: Some(component(catalog)),
        relay_billing: Some(billing.into()),
    };
    let status = if health.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(health)).into_response()
}

fn component(healthy: bool) -> String {
    if healthy { "ok" } else { "error" }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_status_is_stable() {
        assert_eq!(component(true), "ok");
        assert_eq!(component(false), "error");
    }
}

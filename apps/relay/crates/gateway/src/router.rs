//! Router construction and middleware for the TensorZero Gateway.
//!
//! This module builds the final Axum router with all layers (auth, tracing, metrics)
//! and defines authentication and version header middlewares.

use crate::routes::build_api_routes;
use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use http_body_util::{BodyStream, StreamBody};
use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::Arc;
use tensorzero_auth::middleware::TensorzeroAuthMiddlewareStateInner;
use tensorzero_core::observability::TracerWrapper;
use tensorzero_core::observability::request_logging::InFlightRequestsData;
#[expect(
    clippy::disallowed_types,
    reason = "gateway router construction holds SwappableAppStateData as root axum state"
)]
use tensorzero_core::{endpoints, utils::gateway::SwappableAppStateData};
use tensorzero_core::{
    endpoints::TensorzeroAuthMiddlewareState,
    error::{Error, ErrorDetails},
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tokio_util::sync::CancellationToken;
use tower_http::decompression::RequestDecompressionLayer;
use tracing::Instrument;

/// Builds the final Axum router for the gateway,
/// which can be passed to `axum::serve` to start the server.
#[expect(
    clippy::disallowed_types,
    reason = "router construction receives SwappableAppStateData from GatewayHandle"
)]
pub async fn build_axum_router(
    base_path: &str,
    otel_tracer: Option<Arc<TracerWrapper>>,
    app_state: SwappableAppStateData,
    metrics_handle: PrometheusHandle,
    shutdown_token: CancellationToken,
) -> Result<(Router, InFlightRequestsData), Error> {
    let api_routes = build_api_routes(otel_tracer, metrics_handle);
    // The path was just `/` (or multiple slashes)
    let mut router = if base_path.is_empty() {
        Router::new().merge(api_routes)
    } else {
        Router::new().nest(base_path, api_routes)
    };

    // Serve the MCP endpoint on the same port, respecting `base_path`.
    let mcp_router =
        tensorzero_mcp::build_mcp_router(Arc::new(app_state.clone()), shutdown_token.clone())
            .await
            .map_err(|e| Error::new(ErrorDetails::InternalError { message: e }))?;
    let mcp_path = if base_path.is_empty() {
        "/mcp".to_string()
    } else {
        format!("{base_path}/mcp")
    };
    router = router
        .nest_service(&mcp_path, mcp_router)
        .fallback(endpoints::fallback::handle_404);

    // Success-only billing hook (`relay-billing`). Applied BEFORE the auth layer
    // below so that — since `.layer` composes inner-to-outer — auth runs first
    // and the authenticated API-key identity (`RequestApiKeyExtension`) is
    // already in the request extensions when the billing middleware resolves the
    // `AccountId`. The middleware is a no-op for non-inference routes and for
    // unauthenticated requests.
    //
    // Billing is OFF by default and only wired when `RELAY_BILLING_ENABLED` is
    // set. When `MINT_SERVER_URL` is present, the engine is a real,
    // mint-server-backed `MintLedger` (holds/captures/releases over HTTP against a
    // durable, funded ledger) — the PRODUCTION backend. When `MINT_SERVER_URL` is
    // missing or invalid billing configuration aborts startup. Operators can
    // explicitly disable billing for local gateway development.
    let config = app_state.config().load();
    let catalog_runtime = build_catalog_runtime().await?;
    router = router.layer(Extension(catalog_runtime.service.clone()));
    let mut relay_health =
        crate::relay_health::RelayHealthState::new(catalog_runtime.service.clone());
    let cost_event_store: Arc<dyn relay_cost_feed::CostEventStore> = app_state
        .postgres_connection_info()
        .get_pool()
        .cloned()
        .map_or_else(
            || Arc::new(relay_cost_feed::InMemoryCostEventStore::default()) as Arc<_>,
            |pool| Arc::new(crate::cost_feed::PostgresCostEventStore::new(pool)) as Arc<_>,
        );
    router = router.layer(Extension(cost_event_store));

    // Adaptive provider health is deliberately outside the hot request path:
    // restore one validated checkpoint before serving, then persist consistent
    // snapshots periodically and once more during graceful shutdown. The
    // gateway can still serve without Postgres or after a checkpoint failure;
    // this state optimizes routing but is never request or billing authority.
    if let Some(pool) = app_state.postgres_connection_info().get_pool().cloned() {
        match crate::provider_health_persistence::restore(&pool).await {
            Ok(restored) if restored > 0 => {
                tracing::info!(restored, "Restored Relay provider-health snapshots");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Relay provider-health warm start unavailable; starting with neutral scores"
                );
            }
        }

        let persistence_shutdown = shutdown_token.clone();
        app_state.deferred_tasks.spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Avoid an immediate write of the just-restored state.
            interval.tick().await;
            loop {
                tokio::select! {
                    () = persistence_shutdown.cancelled() => {
                        if let Err(error) = crate::provider_health_persistence::persist(&pool).await {
                            tracing::warn!(error = %error, "Final Relay provider-health checkpoint failed");
                        }
                        break;
                    }
                    _ = interval.tick() => {
                        if let Err(error) = crate::provider_health_persistence::persist(&pool).await {
                            tracing::warn!(error = %error, "Relay provider-health checkpoint failed");
                        }
                    }
                }
            }
        });
    }

    let billing_on = crate::billing::billing_enabled();
    let metering_on = !billing_on && crate::metering::metering_enabled();
    let postgres_pool = app_state.postgres_connection_info().get_pool().cloned();

    // Operator admin surfaces (catalog price editor, provider keys) persist in
    // the Relay schema. Apply it whenever Postgres is configured; a failure
    // only disables those editors and never blocks inference.
    let relay_schema_ready = match &postgres_pool {
        Some(pool) => match relay_storage_postgres::migrate(pool).await {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    "Relay schema migration failed; price editing and provider key management are disabled"
                );
                false
            }
        },
        None => false,
    };
    let admin_pool = postgres_pool.clone().filter(|_| relay_schema_ready);
    let default_price = catalog_runtime
        .default_price
        .unwrap_or_else(crate::billing::default_model_price);
    let live_prices = crate::live_prices::LivePrices::new(Arc::new(
        catalog_runtime
            .service
            .billing_price_table(default_price)
            .await,
    ));
    let catalog_admin = crate::catalog_admin::CatalogAdmin::new(
        catalog_runtime.service.clone(),
        catalog_runtime.service.snapshot().await,
        admin_pool
            .clone()
            .map(relay_storage_postgres::PostgresCatalogPriceStore::new),
        live_prices.clone(),
        default_price,
    );
    if let Err(error) = catalog_admin.reload().await {
        tracing::error!(error = %error, "Relay operator price edits could not be applied at startup; serving deployed catalog prices");
    }
    catalog_admin.spawn_refresh(&app_state.deferred_tasks, shutdown_token.clone());
    if let Some((source, interval)) = catalog_feed_refresh(&catalog_admin.base())? {
        tracing::info!(
            url = %source.url(),
            interval_secs = interval.as_secs(),
            "Relay catalog list prices refresh from a model price feed"
        );
        catalog_admin.spawn_feed_refresh(
            &app_state.deferred_tasks,
            shutdown_token.clone(),
            source,
            interval,
        );
    }
    router = router.layer(Extension(catalog_admin));

    let provider_keys = crate::provider_keys::ProviderKeys::new(
        admin_pool
            .clone()
            .map(relay_storage_postgres::PostgresProviderCredentialStore::new),
        crate::provider_keys::master_key_from_env().map_err(billing_config_error)?,
    );
    if let Err(error) = provider_keys.reload().await {
        tracing::error!(error = %error, "Relay stored provider keys could not be loaded; using environment keys");
    }
    provider_keys.spawn_refresh(&app_state.deferred_tasks, shutdown_token.clone());
    // Innermost Relay layer: fills TensorZero `dynamic::` provider credentials
    // right before the inference handler parses the request.
    router = router.layer(middleware::from_fn_with_state(
        provider_keys.clone(),
        crate::provider_keys::provider_credentials_middleware,
    ));
    router = router.layer(Extension(provider_keys));
    let runtime_status = crate::relay_access::RelayRuntimeStatus {
        billing_enabled: billing_on,
        metering_enabled: metering_on,
        auth_enabled: config.gateway.auth.enabled,
        postgres_configured: postgres_pool.is_some(),
        budgets_enforced: (billing_on || metering_on) && postgres_pool.is_some(),
        cost_feed: crate::cost_feed::cost_feed_status(),
    };
    if billing_on && crate::metering::metering_enabled() {
        tracing::info!(
            "RELAY_METERING_ENABLED is ignored while RELAY_BILLING_ENABLED is set; Mint-settled billing already emits cost events"
        );
    }

    if billing_on {
        if !config.gateway.auth.enabled {
            return Err(billing_config_error(
                "RELAY_BILLING_ENABLED requires gateway authentication; unauthenticated inference cannot resolve a payer",
            ));
        }
        let ledger: Arc<dyn relay_billing::BillingLedger> = build_billing_ledger()?;
        relay_health = relay_health.with_billing(ledger.clone());
        router = router.layer(Extension(crate::relay_access::WalletLedger(ledger.clone())));
        validate_configured_catalog_routes(&catalog_runtime.service, &config).await?;
        // Model-aware pricing: a per-model price table seeded from the real
        // provider list-price catalog (`relay_billing::catalog`), so each model is
        // billed at its own accurate per-token micro-USD rate. Any model not in
        // the catalog falls back to the conservative safe default — never one flat
        // rate for every model.
        //
        // Operators can point `RELAY_PRICE_CATALOG_PATH` at a JSON file to
        // add/override per-model prices (and the default) without recompiling; it
        // is overlaid on the embedded catalog and validated at startup, so a
        // malformed file fails closed rather than mispricing a money path. When
        // unset, the embedded catalog is used unchanged (backward-compatible).
        // Provider hedging and adaptive SLO routing are configured per model and
        // execute inside TensorZero's provider dispatch before this middleware
        // observes the single winning response.
        let prices = live_prices.current();
        let payee = std::env::var("RELAY_AITP_PAYEE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                billing_config_error("RELAY_BILLING_ENABLED requires RELAY_AITP_PAYEE")
            })?;
        ledger
            .validate_payee_address(&payee)
            .await
            .map_err(|error| {
                billing_config_error(format!(
                    "RELAY_AITP_PAYEE does not map to MINT_PAYEE_ACCOUNT: {error}"
                ))
            })?;
        let evidence_pool = app_state
            .postgres_connection_info()
            .get_pool()
            .cloned()
            .ok_or_else(|| {
                billing_config_error(
                    "RELAY_BILLING_ENABLED requires Postgres for durable settlement evidence",
                )
            })?;
        relay_storage_postgres::migrate(&evidence_pool)
            .await
            .map_err(|error| {
                billing_config_error(format!(
                    "failed to apply the restart-durable Relay schema: {error}"
                ))
            })?;
        let evidence_store = Arc::new(
            crate::settlement_evidence::PostgresSettlementEvidenceStore::new(evidence_pool.clone()),
        );
        let challenge_issuer = Arc::new(relay_aitp::ChallengeIssuer::new(
            relay_aitp::ChallengePolicy {
                payee: payee.clone(),
                ttl_ms: crate::aitp::AITP_CHALLENGE_TTL_MS,
                max_amount_micros: relay_contract::Microdollars::new(i64::MAX),
            },
            Arc::new(crate::aitp_challenge_store::PostgresChallengeStore::new(
                evidence_pool.clone(),
            )),
            Arc::new(relay_aitp::SystemClock),
            Arc::new(relay_aitp::UuidNonceSource),
        ));
        let challenge_cleanup = challenge_issuer.clone();
        let challenge_cleanup_shutdown = shutdown_token.clone();
        app_state.deferred_tasks.spawn(async move {
            const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
            const BATCH_SIZE: usize = 1_000;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3_600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Consume Tokio's immediate first tick; cleanup itself runs now.
            interval.tick().await;
            // Cleanup on startup, hourly, and once during graceful shutdown.
            loop {
                let cutoff = relay_billing::now_ms().saturating_sub(RETENTION_MS);
                if let Err(error) = challenge_cleanup.purge_before(cutoff, BATCH_SIZE).await {
                    tracing::warn!(error = %error, "Relay AITP retention cleanup failed");
                }
                tokio::select! {
                    () = challenge_cleanup_shutdown.cancelled() => {
                        let cutoff = relay_billing::now_ms().saturating_sub(RETENTION_MS);
                        if let Err(error) = challenge_cleanup.purge_before(cutoff, BATCH_SIZE).await {
                            tracing::warn!(error = %error, "Final Relay AITP retention cleanup failed");
                        }
                        break;
                    },
                    _ = interval.tick() => {}
                }
            }
        });
        crate::cost_feed::start_if_configured(
            evidence_pool.clone(),
            app_state.deferred_tasks.clone(),
            shutdown_token.clone(),
        )
        .map_err(|error| {
            billing_config_error(format!("failed to initialize Relay cost feed: {error}"))
        })?;
        let coordinator = Arc::new(
            crate::settlement_outbox::DurableBillingCoordinator::postgres(
                ledger.clone(),
                evidence_pool,
            ),
        );
        coordinator.recover_once(100).await.map_err(|error| {
            billing_config_error(format!("Relay billing recovery failed at startup: {error}"))
        })?;
        let recovery = coordinator.clone();
        let recovery_shutdown = shutdown_token.clone();
        app_state.deferred_tasks.spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = recovery_shutdown.cancelled() => {
                        // One final bounded reconciliation pass drains commands
                        // staged by requests that completed during shutdown.
                        if let Err(error) = recovery.recover_once(100).await {
                            tracing::error!(error = %error, "Final Relay billing recovery pass failed");
                        }
                        break;
                    }
                    _ = interval.tick() => {
                        if let Err(error) = recovery.recover_once(100).await {
                            tracing::error!(error = %error, "Relay billing recovery pass failed");
                        }
                    }
                }
            }
        });
        let billing_state = crate::billing::BillingState::new(
            ledger,
            crate::billing::EstimatePolicy::default(),
            prices,
            payee,
        )
        .with_evidence_store(evidence_store)
        .with_challenge_issuer(challenge_issuer)
        .with_coordinator(coordinator)
        .with_live_prices(live_prices.clone())
        .with_deferred_tasks(app_state.deferred_tasks.clone());
        router = router.layer(middleware::from_fn_with_state(
            billing_state,
            crate::billing::billing_middleware,
        ));
    }
    if metering_on {
        router = build_metering_layer(
            router,
            &catalog_runtime,
            live_prices.clone(),
            &config,
            postgres_pool.clone(),
            &app_state,
            &shutdown_token,
        )
        .await?;
    }
    // Per-key spend caps wrap the billing/metering layer (outer) but stay
    // inside auth, so the capped principal is already resolved and a capped key
    // never reaches AITP, Mint, or a provider.
    if let (true, Some(pool)) = (runtime_status.budgets_enforced, postgres_pool) {
        router = router.layer(middleware::from_fn_with_state(
            crate::metering::SpendCapState::new(
                relay_storage_postgres::PostgresKeyBudgetStore::new(pool),
            ),
            crate::metering::spend_cap_middleware,
        ));
    }
    router = router
        .layer(Extension(relay_health))
        .layer(Extension(runtime_status));

    if config.gateway.auth.enabled {
        let state = TensorzeroAuthMiddlewareState::new(TensorzeroAuthMiddlewareStateInner {
            unauthenticated_routes: UNAUTHENTICATED_ROUTES,
            auth_cache: app_state.auth_cache.clone(),
            pool: app_state.postgres_connection_info().get_pool().cloned(),
            error_json: config.gateway.unstable_error_json,
            base_path: (!base_path.is_empty()).then(|| base_path.to_string()),
            // The standalone gateway always reads the API key from the `Authorization`
            // header. Other consumers of `tensorzero-auth` may set this to a different
            // header name.
            auth_header: None,
        });
        router = router.layer(middleware::from_fn_with_state(
            state,
            tensorzero_auth::middleware::tensorzero_auth_middleware,
        ));
    }

    let in_flight_requests_data =
        tensorzero_core::observability::request_logging::InFlightRequestsData::new();
    // Everything added from this point onwards does *NOT* have authentication applied - that is,
    // it wraps the authentication middleware
    // increase the default body limit from 2MB to 100MB
    let final_router = router
        .layer(axum::middleware::from_fn(add_version_header))
        .layer(DefaultBodyLimit::max(
            crate::billing::MAX_REQUEST_BODY_BYTES,
        ))
        // Accept encoded requests and transparently decompress them.
        // Supported encodings: gzip, br, zstd.
        .layer(RequestDecompressionLayer::new())
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            possibly_prevent_request_cancellation,
        ))
        // This should always be the very last layer in the stack, so that we start counting as soon as we begin processing a request
        .layer(axum::middleware::from_fn_with_state(
            in_flight_requests_data.clone(),
            tensorzero_core::observability::request_logging::request_logging_middleware,
        ))
        .with_state(app_state.clone());
    Ok((final_router, in_flight_requests_data))
}

/// Wire postpaid meter-only usage recording (`RELAY_METERING_ENABLED`).
///
/// Fail-closed like billing: metering without authentication cannot attribute
/// usage to an API key, and without Postgres there is no durable outbox.
#[expect(
    clippy::disallowed_types,
    reason = "metering wiring reads deferred tasks from SwappableAppStateData"
)]
async fn build_metering_layer(
    router: Router<SwappableAppStateData>,
    catalog_runtime: &CatalogRuntime,
    prices: crate::live_prices::LivePrices,
    config: &tensorzero_core::config::Config,
    pool: Option<sqlx::PgPool>,
    app_state: &SwappableAppStateData,
    shutdown_token: &CancellationToken,
) -> Result<Router<SwappableAppStateData>, Error> {
    if !config.gateway.auth.enabled {
        return Err(billing_config_error(
            "RELAY_METERING_ENABLED requires gateway authentication; set `[gateway.auth] enabled = true` so usage is attributed to an API key",
        ));
    }
    let pool = pool.ok_or_else(|| {
        billing_config_error(
            "RELAY_METERING_ENABLED requires Postgres for the durable cost-event outbox",
        )
    })?;
    relay_storage_postgres::migrate(&pool)
        .await
        .map_err(|error| {
            billing_config_error(format!(
                "failed to apply the Relay metering schema: {error}"
            ))
        })?;
    validate_configured_catalog_routes(&catalog_runtime.service, config).await?;
    crate::cost_feed::start_if_configured(
        pool.clone(),
        app_state.deferred_tasks.clone(),
        shutdown_token.clone(),
    )
    .map_err(|error| {
        billing_config_error(format!("failed to initialize Relay cost feed: {error}"))
    })?;
    let store: Arc<dyn relay_cost_feed::CostEventStore> =
        Arc::new(crate::cost_feed::PostgresCostEventStore::new(pool.clone()));
    tracing::info!("Relay postpaid metering enabled; usage is recorded without Mint holds");
    Ok(router.layer(middleware::from_fn_with_state(
        crate::metering::MeteringState::new(store, prices, app_state.deferred_tasks.clone())
            .with_batches(relay_storage_postgres::PostgresMeteredBatchStore::new(pool)),
        crate::metering::metering_middleware,
    )))
}

async fn validate_configured_catalog_routes(
    catalog: &relay_catalog::CatalogService,
    config: &tensorzero_core::config::Config,
) -> Result<(), Error> {
    let model_routes = config
        .models
        .table
        .keys()
        .map(|name| format!("tensorzero::model_name::{name}"));
    let embedding_routes = config
        .embedding_models
        .table
        .keys()
        .map(|name| format!("tensorzero::model_name::{name}"));
    // TensorZero injects reserved built-in helper functions. They are not
    // operator-callable retail routes and therefore do not belong in Relay's
    // operator price catalog.
    let function_routes = config
        .functions
        .keys()
        .filter(|name| !name.starts_with("tensorzero::"))
        .map(|name| format!("tensorzero::function_name::{name}"));
    catalog
        .validate_configured_routes(model_routes.chain(embedding_routes).chain(function_routes))
        .await
        .map_err(|error| {
            billing_config_error(format!(
                "Relay catalog does not cover the configured inference surface: {error}"
            ))
        })
}

/// Build the PRODUCTION billing engine behind the `Arc<dyn BillingLedger>` seam.
///
/// Billing is a money path, so configuration is fail-closed: opting in without
/// a durable Mint backend and its credentials is a startup error.
fn build_billing_ledger() -> Result<Arc<dyn relay_billing::BillingLedger>, Error> {
    let mint_url = std::env::var("MINT_SERVER_URL")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());

    let Some(url) = mint_url else {
        return Err(billing_config_error(
            "RELAY_BILLING_ENABLED requires MINT_SERVER_URL",
        ));
    };

    let api_key = std::env::var("MINT_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Err(billing_config_error(
            "RELAY_BILLING_ENABLED requires MINT_API_KEY",
        ));
    }
    let payee = std::env::var("MINT_PAYEE_ACCOUNT").unwrap_or_default();
    if payee.is_empty() {
        return Err(billing_config_error(
            "RELAY_BILLING_ENABLED requires MINT_PAYEE_ACCOUNT",
        ));
    }
    let payer = std::env::var("MINT_PAYER_ACCOUNT")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());

    #[derive(serde::Deserialize)]
    struct VerificationKeyConfig {
        key_id: String,
        public_key_hex: String,
    }
    let raw_keys = std::env::var("MINT_VERIFICATION_KEYS_JSON")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            billing_config_error(
                "RELAY_BILLING_ENABLED requires MINT_VERIFICATION_KEYS_JSON with pinned Ed25519 keys",
            )
        })?;
    let configured: Vec<VerificationKeyConfig> =
        serde_json::from_str(&raw_keys).map_err(|error| {
            billing_config_error(format!("invalid MINT_VERIFICATION_KEYS_JSON: {error}"))
        })?;
    let mut verification_keys = Vec::with_capacity(configured.len());
    for configured_key in configured {
        if configured_key.key_id.trim().is_empty() {
            return Err(billing_config_error(
                "MINT_VERIFICATION_KEYS_JSON key_id cannot be empty",
            ));
        }
        let bytes = hex::decode(configured_key.public_key_hex.trim()).map_err(|error| {
            billing_config_error(format!("invalid Mint verification key hex: {error}"))
        })?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            billing_config_error("Mint verification public keys must decode to 32 bytes")
        })?;
        verification_keys.push((configured_key.key_id, bytes));
    }

    match relay_billing::MintLedger::with_reqwest_verified(
        &url,
        api_key,
        payee,
        payer,
        verification_keys,
    ) {
        Ok(ledger) => {
            tracing::info!(mint_server_url = %url, "relay-billing settling on mint-server");
            Ok(Arc::new(ledger))
        }
        Err(error) => Err(billing_config_error(format!(
            "failed to initialize Mint billing client: {error}"
        ))),
    }
}

fn billing_config_error(message: impl Into<String>) -> Error {
    Error::new(ErrorDetails::Config {
        message: message.into(),
    })
}

struct CatalogRuntime {
    service: Arc<relay_catalog::CatalogService>,
    default_price: Option<relay_billing::ModelPrice>,
}

async fn build_catalog_runtime() -> Result<CatalogRuntime, Error> {
    let catalog_path = nonempty_env("RELAY_CATALOG_PATH");
    let legacy_path = nonempty_env("RELAY_PRICE_CATALOG_PATH");
    if catalog_path.is_some() && legacy_path.is_some() {
        return Err(billing_config_error(
            "set only one of RELAY_CATALOG_PATH or RELAY_PRICE_CATALOG_PATH",
        ));
    }
    let generated_at_ms = relay_billing::now_ms();
    if let Some(path) = catalog_path {
        let json = std::fs::read_to_string(&path).map_err(|error| {
            billing_config_error(format!(
                "failed to read RELAY_CATALOG_PATH '{path}': {error}"
            ))
        })?;
        let service = relay_catalog::CatalogService::initialize(Arc::new(
            relay_catalog::JsonCatalogSource::new(json),
        ))
        .await
        .map_err(|error| {
            billing_config_error(format!("failed to initialize Relay catalog: {error}"))
        })?;
        return Ok(CatalogRuntime {
            service: Arc::new(service),
            default_price: None,
        });
    }
    if let Some(path) = legacy_path {
        let json = std::fs::read_to_string(&path).map_err(|error| {
            billing_config_error(format!(
                "failed to read RELAY_PRICE_CATALOG_PATH '{path}': {error}"
            ))
        })?;
        let config = relay_billing::catalog_config::parse(&json).map_err(|error| {
            billing_config_error(format!(
                "failed to load RELAY_PRICE_CATALOG_PATH '{path}': {error}"
            ))
        })?;
        let default_price = config.default.map(|price| {
            relay_billing::ModelPrice::new(
                price.input_micro_usd_per_million,
                price.output_micro_usd_per_million,
            )
        });
        let service =
            relay_catalog::CatalogService::builtin_with_price_overrides(generated_at_ms, &config)
                .await
                .map_err(|error| {
                    billing_config_error(format!("failed to initialize Relay catalog: {error}"))
                })?;
        return Ok(CatalogRuntime {
            service: Arc::new(service),
            default_price,
        });
    }
    let service = relay_catalog::CatalogService::builtin(generated_at_ms)
        .await
        .map_err(|error| {
            billing_config_error(format!("failed to initialize Relay catalog: {error}"))
        })?;
    Ok(CatalogRuntime {
        service: Arc::new(service),
        default_price: None,
    })
}

/// Default and minimum interval between catalog feed refreshes.
const CATALOG_REFRESH_DEFAULT_SECS: u64 = 3_600;
const CATALOG_REFRESH_MIN_SECS: u64 = 300;

/// Opt-in list-price refresh from OpenRouter's public model feed:
/// `RELAY_CATALOG_REFRESH_URL` (`openrouter` or `default` selects
/// `https://openrouter.ai/api/v1/models`), `RELAY_CATALOG_REFRESH_INTERVAL_SECS`
/// (default 3600, minimum 300) and `RELAY_CATALOG_REFRESH_ADD_MODELS` (truthy
/// also lists feed models the deployed catalog does not have).
fn catalog_feed_refresh(
    base: &relay_catalog::CatalogSnapshot,
) -> Result<Option<(relay_catalog::HttpOpenRouterCatalogSource, std::time::Duration)>, Error> {
    let Some(raw_url) = nonempty_env("RELAY_CATALOG_REFRESH_URL") else {
        return Ok(None);
    };
    let url = if raw_url.eq_ignore_ascii_case("openrouter") || raw_url.eq_ignore_ascii_case("default")
    {
        relay_catalog::OPENROUTER_MODELS_URL.to_string()
    } else {
        raw_url
    };
    let interval_secs = match nonempty_env("RELAY_CATALOG_REFRESH_INTERVAL_SECS") {
        None => CATALOG_REFRESH_DEFAULT_SECS,
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            billing_config_error(format!(
                "RELAY_CATALOG_REFRESH_INTERVAL_SECS must be a whole number of seconds, got '{raw}'"
            ))
        })?,
    };
    if interval_secs < CATALOG_REFRESH_MIN_SECS {
        return Err(billing_config_error(format!(
            "RELAY_CATALOG_REFRESH_INTERVAL_SECS must be at least {CATALOG_REFRESH_MIN_SECS}"
        )));
    }
    let add_new_models = nonempty_env("RELAY_CATALOG_REFRESH_ADD_MODELS").is_some_and(|value| {
        matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
    });
    let source = relay_catalog::HttpOpenRouterCatalogSource::new(
        url,
        base.clone(),
        relay_catalog::FeedOptions {
            add_new_models,
            ..relay_catalog::FeedOptions::default()
        },
    )
    .map_err(|error| billing_config_error(format!("invalid RELAY_CATALOG_REFRESH_URL: {error}")))?;
    Ok(Some((source, std::time::Duration::from_secs(interval_secs))))
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Routes that should not require authentication
/// We apply authentication to all routes *except* these ones, to make it difficult
/// to accidentally skip running authentication on a route, especially if we later refactor
/// how we build up our router.
const UNAUTHENTICATED_ROUTES: &[&str] = &["/status", "/health", "/internal/autopilot/status"];

/// This middleware spawns all non-'safe' (e.g. non GET/HEAD/OPTIONS) requests on a new tokio task,
/// and awaits the task completion. This prevents the handler future from getting dropped if
/// the client closes the connection early.
///
/// This ensures that POST requests (e.g. POST /inference) continue to run even if the client closes the connection early.
/// so that we perform all of our desired side effects (writing to the database, handling rate limiting, etc.)
///
/// Note that it's inherently impossible to guarantee that we *start* processing a request,
/// since the client might close the connection before sending the complete HTTP request
/// (e.g. it doesn't write all of the headers or body).
/// We guarantee that if we *start* processing a request, we will run our handler/stream logic to completion,
/// including all our side effects (writing to the database, handling rate limiting, etc.).
#[expect(
    clippy::disallowed_types,
    reason = "middleware needs SwappableAppStateData directly to access shutdown_token before FromRef resolves"
)]
async fn possibly_prevent_request_cancellation(
    State(state): State<SwappableAppStateData>,
    request: Request,
    next: Next,
) -> Response {
    if request.method().is_safe() {
        return next.run(request).await;
    }

    let deferred_tasks = state.deferred_tasks.clone();

    // Capture the current tracing span so that the spawned task inherits the span context.
    // This is critical for the `OverheadTimingLayer`, which relies on span parent-child
    // relationships to detect external spans and subtract their time from the overhead metric.
    let span = tracing::Span::current();
    let task = async move {
        let resp = next.run(request).await;
        let (parts, body) = resp.into_parts();
        let mut stream = BodyStream::new(body);
        // We spawn a separate tokio task to drive the stream to completion
        // To match the behavior of the original stream, we only buffer a single frame
        // (meaning that the `send` call will suspend until axum processes the previous frame from the `StreamBody`
        // on the other end).
        // This ensures that the stream will always be driven to completion, even if the client closes the connection early.
        let (send, recv) = tokio::sync::mpsc::channel(1);
        let inner_span = tracing::Span::current();
        deferred_tasks.spawn(
            async move {
                while let Some(chunk) = stream.next().await {
                    // We deliberately ignore errors here - even if the receiver is dropped
                    // (due to the client closing the connection early), we still want to
                    // drive the original stream to completion, which will finish executing
                    // all of the tensorzero stream logic (e.g. writing the collected chunks
                    // to the database, handling rate limiting, etc.)
                    let _ = send.send(chunk).await;
                }
            }
            .instrument(inner_span),
        );
        let spawned_stream = axum::body::Body::new(StreamBody::new(ReceiverStream::new(recv)));
        Response::from_parts(parts, spawned_stream)
    }
    .instrument(span);
    // Our 'task' includes the call to 'next.run(request)', which ensures that the request handler
    // will still be executed to completion, even if the client closes the connection early.
    // The 'task' future includes additional handling for streaming responses
    match state.deferred_tasks.spawn(task).await {
        Ok(resp) => resp,
        Err(e) => Error::new(ErrorDetails::InternalError {
            message: format!("Failed to join task in possibly_prevent_request_cancellation: {e:?}"),
        })
        .into_response(),
    }
}

async fn add_version_header(request: Request, next: Next) -> Response {
    #[cfg_attr(not(feature = "e2e_tests"), expect(unused_mut))]
    let mut version = axum::http::HeaderValue::from_static(endpoints::status::TENSORZERO_VERSION);

    #[cfg(feature = "e2e_tests")]
    {
        if request
            .headers()
            .contains_key("x-tensorzero-e2e-version-remove")
        {
            tracing::info!("Removing version header due to e2e header");
            return next.run(request).await;
        }
        if let Some(header_version) = request.headers().get("x-tensorzero-e2e-version-override") {
            tracing::info!("Overriding version header with e2e header: {header_version:?}");
            version = header_version.clone();
        }
    }

    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-tensorzero-gateway-version", version);
    response
}

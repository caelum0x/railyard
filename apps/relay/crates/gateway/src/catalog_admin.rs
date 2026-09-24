//! Operator retail price editor for the Relay catalog.
//!
//! Price edits are persisted in `relay_catalog_price_overrides` and overlaid
//! on the deployed catalog (`RELAY_CATALOG_PATH`, the legacy price file, or the
//! embedded list prices). Applying an edit updates the discovery/quote catalog
//! and swaps the live billing/metering [`LivePrices`] table in one step, so the
//! next request is priced at the new rate without a redeploy. Every replica
//! re-reads the table periodically, so edits made through one replica reach
//! the others within [`PRICE_REFRESH_INTERVAL`].
//!
//! When `RELAY_CATALOG_REFRESH_URL` is set, the deployed base itself is
//! refreshed on an interval from OpenRouter's public model list (list-priced
//! rows only). Operator edits are re-applied on top after every feed refresh,
//! so an operator price always wins over the feed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_catalog::{CatalogError, CatalogModel, CatalogService, CatalogSnapshot};
use relay_contract::{ApiError, ApiErrorEnvelope, DataEnvelope};
use relay_storage_postgres::{
    CatalogPriceError, CatalogPriceInput, CatalogPriceOverride, PostgresCatalogPriceStore,
};
use serde::{Deserialize, Serialize};
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::live_prices::LivePrices;
use crate::relay_access::{is_operator, require_operator};

/// How often each replica re-reads operator price edits from Postgres.
pub const PRICE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Shared price-editor state installed as an axum extension.
#[derive(Clone)]
pub struct CatalogAdmin {
    catalog: Arc<CatalogService>,
    /// The deployed catalog before any operator edit; edits always overlay
    /// this so removing an edit restores the deployed price exactly. The feed
    /// refresher swaps in a repriced base.
    base: Arc<std::sync::RwLock<Arc<CatalogSnapshot>>>,
    store: Option<PostgresCatalogPriceStore>,
    prices: LivePrices,
    default_price: relay_billing::ModelPrice,
    /// Serializes apply so a slow refresh cannot overwrite a newer edit.
    apply_lock: Arc<Mutex<()>>,
}

impl CatalogAdmin {
    pub fn new(
        catalog: Arc<CatalogService>,
        base: CatalogSnapshot,
        store: Option<PostgresCatalogPriceStore>,
        prices: LivePrices,
        default_price: relay_billing::ModelPrice,
    ) -> Self {
        Self {
            catalog,
            base: Arc::new(std::sync::RwLock::new(Arc::new(base))),
            store,
            prices,
            default_price,
            apply_lock: Arc::new(Mutex::new(())),
        }
    }

    /// The deployed base currently in effect (before operator edits).
    pub fn base(&self) -> Arc<CatalogSnapshot> {
        match self.base.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_base(&self, base: CatalogSnapshot) {
        let base = Arc::new(base);
        match self.base.write() {
            Ok(mut guard) => *guard = base,
            Err(poisoned) => *poisoned.into_inner() = base,
        }
    }

    /// Re-read persisted edits and publish them to the catalog and billing.
    pub async fn reload(&self) -> Result<CatalogSnapshot, String> {
        let _guard = self.apply_lock.lock().await;
        self.reload_locked().await
    }

    async fn reload_locked(&self) -> Result<CatalogSnapshot, String> {
        let overrides = match &self.store {
            Some(store) => store.list().await.map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        self.apply(&overrides).await
    }

    /// Merge the feed onto the current base, publish it, and re-apply operator
    /// edits on top. A failed fetch or an invalid merge leaves the last-good
    /// base and prices serving.
    pub async fn refresh_from_feed(
        &self,
        source: &relay_catalog::HttpOpenRouterCatalogSource,
    ) -> Result<relay_catalog::MergeReport, String> {
        let _guard = self.apply_lock.lock().await;
        let current = self.base();
        let (merged, report) = source
            .merge_onto(&current, relay_billing::now_ms())
            .await
            .map_err(|error| error.to_string())?;
        self.replace_base_locked(merged).await?;
        Ok(report)
    }

    /// Swap in a new deployed base and re-apply operator edits on top.
    #[cfg(test)]
    pub async fn replace_base(&self, base: CatalogSnapshot) -> Result<CatalogSnapshot, String> {
        let _guard = self.apply_lock.lock().await;
        self.replace_base_locked(base).await
    }

    async fn replace_base_locked(&self, base: CatalogSnapshot) -> Result<CatalogSnapshot, String> {
        base.validate().map_err(|error| error.to_string())?;
        let previous = (*self.base()).clone();
        self.set_base(base);
        match self.reload_locked().await {
            Ok(applied) => Ok(applied),
            Err(error) => {
                // Never leave a half-applied refresh: restore the last-good base.
                self.set_base(previous);
                Err(error)
            }
        }
    }

    /// Refresh the base from the configured feed every `interval` (first run
    /// immediately, in the background, so startup never waits on the network).
    pub fn spawn_feed_refresh(
        &self,
        tasks: &TaskTracker,
        shutdown: CancellationToken,
        source: relay_catalog::HttpOpenRouterCatalogSource,
        interval: Duration,
    ) {
        let admin = self.clone();
        tasks.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        match admin.refresh_from_feed(&source).await {
                            Ok(report) => tracing::info!(
                                url = %source.url(),
                                repriced = report.repriced.len(),
                                added = report.added.len(),
                                skipped = report.skipped,
                                suspicious_zero = ?report.suspicious_zero,
                                "Relay catalog refreshed from the model price feed"
                            ),
                            Err(error) => tracing::warn!(
                                url = %source.url(),
                                error = %error,
                                "Relay catalog feed refresh failed; keeping the last-good prices"
                            ),
                        }
                    }
                }
            }
        });
    }

    async fn apply(&self, overrides: &[CatalogPriceOverride]) -> Result<CatalogSnapshot, String> {
        let base = self.base();
        let known: std::collections::HashSet<&str> = base
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        let edits: Vec<(String, i64, i64)> = overrides
            .iter()
            .filter(|edit| {
                let present = known.contains(edit.model_id.as_str());
                if !present {
                    tracing::warn!(
                        model = %edit.model_id,
                        "Ignoring a Relay price edit for a model that is no longer in the deployed catalog"
                    );
                }
                present
            })
            .map(|edit| {
                (
                    edit.model_id.clone(),
                    edit.input_micro_usd_per_million,
                    edit.output_micro_usd_per_million,
                )
            })
            .collect();
        let suffix = if edits.is_empty() {
            String::new()
        } else {
            let latest = overrides
                .iter()
                .map(|edit| edit.updated_at_ms)
                .max()
                .unwrap_or_default();
            format!("operator-prices-{latest}")
        };
        let snapshot = self
            .catalog
            .apply_operator_prices(&base, edits, &suffix)
            .await
            .map_err(|error| error.to_string())?;
        self.prices
            .replace(self.catalog.billing_price_table(self.default_price).await);
        Ok(snapshot)
    }

    /// Refresh persisted edits periodically (multi-replica consistency).
    pub fn spawn_refresh(&self, tasks: &TaskTracker, shutdown: CancellationToken) {
        if self.store.is_none() {
            return;
        }
        let admin = self.clone();
        tasks.spawn(async move {
            let mut interval = tokio::time::interval(PRICE_REFRESH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(error) = admin.reload().await {
                            tracing::warn!(error = %error, "Relay catalog price refresh failed; keeping the last applied prices");
                        }
                    }
                }
            }
        });
    }

    fn base_prices(&self) -> HashMap<String, (i64, i64)> {
        self.base()
            .models
            .iter()
            .map(|model| {
                (
                    model.id.clone(),
                    (
                        model.pricing.input_micro_usd_per_million,
                        model.pricing.output_micro_usd_per_million,
                    ),
                )
            })
            .collect()
    }
}

/// One persisted edit next to the deployed price it replaces.
#[derive(Debug, Clone, Serialize)]
pub struct PriceEditView {
    pub model_id: String,
    pub input_micro_usd_per_million: i64,
    pub output_micro_usd_per_million: i64,
    pub deployed_input_micro_usd_per_million: Option<i64>,
    pub deployed_output_micro_usd_per_million: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

/// `GET /v1/relay/catalog/prices` response body.
#[derive(Debug, Clone, Serialize)]
pub struct PriceEditorState {
    /// Whether this deployment can persist price edits (Postgres present).
    pub storage_available: bool,
    /// Whether the calling key may edit prices.
    pub operator: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub deployed_revision: String,
    pub edits: Vec<PriceEditView>,
}

/// `GET /v1/relay/catalog/prices`: persisted price edits and editor access.
pub async fn list_price_edits_handler(
    Extension(admin): Extension<CatalogAdmin>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let operator = principal
        .as_ref()
        .is_some_and(|Extension(principal)| is_operator(&principal.key_info.public_id));
    let base = admin.base_prices();
    let (edits, unavailable_reason) = match &admin.store {
        None => (
            Vec::new(),
            Some(
                "Price editing requires Postgres (TENSORZERO_POSTGRES_URL); prices come from the deployed catalog only."
                    .to_string(),
            ),
        ),
        Some(store) => match store.list().await {
            Ok(edits) => (edits, None),
            Err(error) => {
                tracing::error!(error = %error, "Relay price edit listing failed");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "relay_catalog_prices_unavailable",
                    "price edits could not be loaded",
                );
            }
        },
    };
    let edits = edits
        .into_iter()
        .map(|edit| {
            let deployed = base.get(edit.model_id.as_str()).copied();
            PriceEditView {
                deployed_input_micro_usd_per_million: deployed.map(|(input, _)| input),
                deployed_output_micro_usd_per_million: deployed.map(|(_, output)| output),
                model_id: edit.model_id,
                input_micro_usd_per_million: edit.input_micro_usd_per_million,
                output_micro_usd_per_million: edit.output_micro_usd_per_million,
                note: edit.note,
                updated_by: edit.updated_by,
                updated_at_ms: edit.updated_at_ms,
            }
        })
        .collect();
    Json(DataEnvelope {
        data: PriceEditorState {
            storage_available: admin.store.is_some(),
            operator,
            unavailable_reason,
            deployed_revision: admin.base().revision.clone(),
            edits,
        },
    })
    .into_response()
}

/// `PUT /v1/relay/catalog/prices`: set a model's retail price. Operator only.
pub async fn upsert_price_handler(
    Extension(admin): Extension<CatalogAdmin>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(input): Json<CatalogPriceInput>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let Some(store) = admin.store.clone() else {
        return storage_unavailable();
    };
    if let Err(CatalogPriceError::Invalid(message)) = input.validate() {
        return admin_error(StatusCode::BAD_REQUEST, "relay_invalid_price", &message);
    }
    let Some(model_id) = deployed_model_id(&admin, &input.model).await else {
        return admin_error(
            StatusCode::NOT_FOUND,
            "relay_model_not_found",
            "only models in the deployed catalog can be repriced",
        );
    };
    if let Err(error) = store.upsert(&model_id, &input, &operator).await {
        return match error {
            CatalogPriceError::Invalid(message) => {
                admin_error(StatusCode::BAD_REQUEST, "relay_invalid_price", &message)
            }
            CatalogPriceError::Store(error) => {
                tracing::error!(error = %error, "Relay price edit could not be saved");
                admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "relay_catalog_prices_unavailable",
                    "the price edit could not be saved",
                )
            }
        };
    }
    tracing::info!(
        model = %model_id,
        input = input.input_micro_usd_per_million,
        output = input.output_micro_usd_per_million,
        operator = %operator,
        "Relay operator changed a retail price"
    );
    applied_model_response(&admin, &model_id).await
}

/// Body of `POST /v1/relay/catalog/prices/reset`.
#[derive(Debug, Deserialize)]
pub struct ResetPriceRequest {
    pub model: String,
}

/// `POST /v1/relay/catalog/prices/reset`: drop an edit so the deployed price
/// applies again. Operator only.
pub async fn reset_price_handler(
    Extension(admin): Extension<CatalogAdmin>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(request): Json<ResetPriceRequest>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let Some(store) = admin.store.clone() else {
        return storage_unavailable();
    };
    let model_id = deployed_model_id(&admin, &request.model)
        .await
        .unwrap_or_else(|| request.model.trim().to_string());
    match store.delete(&model_id).await {
        Ok(true) => {}
        Ok(false) => {
            return admin_error(
                StatusCode::NOT_FOUND,
                "relay_price_edit_not_found",
                "this model has no price edit to reset",
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay price edit could not be removed");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_catalog_prices_unavailable",
                "the price edit could not be removed",
            );
        }
    }
    tracing::info!(model = %model_id, operator = %operator, "Relay operator reset a retail price");
    applied_model_response(&admin, &model_id).await
}

async fn deployed_model_id(admin: &CatalogAdmin, raw: &str) -> Option<String> {
    let canonical = admin.catalog.canonical_id(raw).await.ok()?;
    admin
        .base()
        .models
        .iter()
        .any(|model| model.id == canonical)
        .then_some(canonical)
}

async fn applied_model_response(admin: &CatalogAdmin, model_id: &str) -> Response {
    let snapshot = match admin.reload().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(error = %error, "Relay price edit saved but could not be applied");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_catalog_apply_failed",
                "the price edit was saved but could not be applied; it will be retried on the next refresh",
            );
        }
    };
    match snapshot
        .models
        .into_iter()
        .find(|model| model.id == model_id)
    {
        Some(model) => Json(DataEnvelope::<CatalogModel> { data: model }).into_response(),
        None => admin_error(
            StatusCode::NOT_FOUND,
            "relay_model_not_found",
            &CatalogError::NotFound(model_id.to_string()).to_string(),
        ),
    }
}

fn storage_unavailable() -> Response {
    admin_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "relay_catalog_prices_unavailable",
        "price editing requires Postgres (TENSORZERO_POSTGRES_URL)",
    )
}

fn admin_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ApiErrorEnvelope {
            error: ApiError {
                message: message.into(),
                kind: Some("relay_catalog_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn admin() -> CatalogAdmin {
        let catalog = Arc::new(CatalogService::builtin(1).await.unwrap());
        let base = catalog.snapshot().await;
        let prices = LivePrices::new(Arc::new(
            catalog
                .billing_price_table(relay_billing::ModelPrice::new(1, 1))
                .await,
        ));
        CatalogAdmin::new(
            catalog,
            base,
            None,
            prices,
            relay_billing::ModelPrice::new(1, 1),
        )
    }

    #[tokio::test]
    async fn applying_an_edit_reprices_billing_and_quotes() {
        let admin = admin().await;
        admin
            .apply(&[CatalogPriceOverride {
                model_id: "openai/gpt-4o".into(),
                input_micro_usd_per_million: 1_000_000,
                output_micro_usd_per_million: 1_000_000,
                note: None,
                updated_by: "ops".into(),
                updated_at_ms: 5,
            }])
            .await
            .unwrap();
        let quote = admin
            .catalog
            .quote("openai/gpt-4o", 1_000, 0)
            .await
            .unwrap();
        assert_eq!(quote.amount_micros.get(), 1_000);
        assert_eq!(
            admin
                .prices
                .current()
                .price_for(&relay_billing::ModelId::namespace("openai/gpt-4o")),
            relay_billing::ModelPrice::new(1_000_000, 1_000_000)
        );

        // Removing every edit restores the deployed price.
        admin.apply(&[]).await.unwrap();
        let quote = admin
            .catalog
            .quote("openai/gpt-4o", 1_000, 500)
            .await
            .unwrap();
        assert_eq!(quote.amount_micros.get(), 7_500);
    }

    #[tokio::test]
    async fn feed_refreshed_base_reprices_billing_and_rejects_invalid_bases() {
        let admin = admin().await;
        let feed = r#"{"data":[{"id":"openai/gpt-4o","architecture":{"output_modalities":["text"]},"pricing":{"prompt":"0.000002","completion":"0.000008"}}]}"#;
        let options = relay_catalog::FeedOptions {
            min_priced_models: 1,
            ..relay_catalog::FeedOptions::default()
        };
        let (merged, _) =
            relay_catalog::merge_openrouter_feed(&admin.base(), feed, 99, options).unwrap();
        admin.replace_base(merged).await.unwrap();
        assert_eq!(
            admin
                .prices
                .current()
                .price_for(&relay_billing::ModelId::namespace("openai/gpt-4o")),
            relay_billing::ModelPrice::new(2_000_000, 8_000_000)
        );
        assert!(admin.base().revision.ends_with("+openrouter-99"));

        let mut invalid = (*admin.base()).clone();
        invalid.revision.clear();
        assert!(admin.replace_base(invalid).await.is_err());
        assert!(admin.base().revision.ends_with("+openrouter-99"));
    }
}

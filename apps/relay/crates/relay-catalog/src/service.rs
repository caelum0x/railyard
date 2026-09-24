use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::model::canonical_catalog_id;
use crate::{
    CatalogError, CatalogModel, CatalogSnapshot, CatalogSource, ModelCapabilities, ModelPrice,
    PriceQuote, PricingSource, Result,
};

#[derive(Clone)]
pub struct CatalogService {
    source: Arc<dyn CatalogSource>,
    snapshot: Arc<RwLock<CatalogSnapshot>>,
}

impl CatalogService {
    pub async fn initialize(source: Arc<dyn CatalogSource>) -> Result<Self> {
        let snapshot = source.load().await?;
        snapshot.validate()?;
        Ok(Self {
            source,
            snapshot: Arc::new(RwLock::new(snapshot)),
        })
    }

    /// The embedded list-price catalog. Prices come from
    /// [`relay_billing::BUILTIN_PRICES`] (the billing table), and each row is
    /// enriched with the capabilities, display name and limits recorded in the
    /// bundled `data/catalog.json` when it lists the model.
    pub async fn builtin(generated_at_ms: i64) -> Result<Self> {
        let bundled = bundled_snapshot()?;
        let models = relay_billing::BUILTIN_PRICES
            .iter()
            .map(|(id, input, output)| {
                let mut model = builtin_model(id, *input, *output);
                if let Some(rich) = bundled.models.iter().find(|m| m.id == model.id) {
                    model.display_name = rich.display_name.clone();
                    model.capabilities = rich.capabilities;
                    model.context_window_tokens = rich.context_window_tokens;
                    model.max_output_tokens = rich.max_output_tokens;
                }
                model
            })
            .collect();
        let snapshot = CatalogSnapshot::new("relay-builtin-v1", generated_at_ms, models);
        Self::initialize(Arc::new(crate::StaticCatalogSource::new(snapshot))).await
    }

    /// Build the embedded catalog with a validated legacy operator price
    /// overlay. Existing rows retain their capabilities while new rows become
    /// generic chat models. This keeps the discovery API and billing table on
    /// exactly the same effective prices during migration from
    /// `RELAY_PRICE_CATALOG_PATH`.
    pub async fn builtin_with_price_overrides(
        generated_at_ms: i64,
        config: &relay_billing::PriceCatalogConfig,
    ) -> Result<Self> {
        config
            .validate()
            .map_err(|error| CatalogError::Source(error.to_string()))?;
        let service = Self::builtin(generated_at_ms).await?;
        {
            let mut snapshot = service.snapshot.write().await;
            snapshot.revision = "relay-operator-price-overlay-v1".into();
            for entry in &config.models {
                let canonical = canonical_catalog_id(entry.model.trim());
                let pricing = ModelPrice::new(
                    entry.input_micro_usd_per_million,
                    entry.output_micro_usd_per_million,
                    PricingSource::OperatorOverride,
                );
                if let Some(model) = snapshot
                    .models
                    .iter_mut()
                    .find(|model| model.id == canonical)
                {
                    model.pricing = pricing;
                } else {
                    snapshot.models.push(builtin_model(
                        &canonical,
                        pricing.input_micro_usd_per_million,
                        pricing.output_micro_usd_per_million,
                    ));
                    let model = snapshot
                        .models
                        .last_mut()
                        .ok_or_else(|| CatalogError::Source("catalog insertion failed".into()))?;
                    model.pricing.source = PricingSource::OperatorOverride;
                }
            }
            snapshot.validate()?;
        }
        Ok(service)
    }

    /// Publish an already-built candidate (for example a feed merge computed
    /// by the caller). Invalid candidates are rejected and the last-good
    /// snapshot keeps serving.
    pub async fn publish(&self, candidate: CatalogSnapshot) -> Result<CatalogSnapshot> {
        candidate.validate()?;
        *self.snapshot.write().await = candidate.clone();
        Ok(candidate)
    }

    /// Atomically refresh. An invalid source result leaves the last-good
    /// snapshot serving traffic.
    pub async fn refresh(&self) -> Result<CatalogSnapshot> {
        let candidate = self.source.load().await?;
        candidate.validate()?;
        *self.snapshot.write().await = candidate.clone();
        Ok(candidate)
    }

    /// Publish `base` with operator retail price edits applied, atomically.
    ///
    /// Each edit is `(model id or alias, input rate, output rate)` and must
    /// name a model already in `base`: the price editor changes prices, it
    /// never adds routes the gateway cannot serve. Edited rows are marked
    /// [`PricingSource::OperatorOverride`]. An invalid candidate leaves the
    /// last-good snapshot serving traffic.
    pub async fn apply_operator_prices(
        &self,
        base: &CatalogSnapshot,
        edits: impl IntoIterator<Item = (String, i64, i64)>,
        revision_suffix: &str,
    ) -> Result<CatalogSnapshot> {
        let mut candidate = base.clone();
        let mut edited = false;
        for (id, input, output) in edits {
            let canonical = canonical_catalog_id(id.trim());
            let model = candidate
                .models
                .iter_mut()
                .find(|model| {
                    model.id == canonical || model.aliases.iter().any(|alias| alias == &canonical)
                })
                .ok_or_else(|| CatalogError::NotFound(canonical.clone()))?;
            model.pricing = ModelPrice::new(input, output, PricingSource::OperatorOverride);
            edited = true;
        }
        if edited && !revision_suffix.is_empty() {
            candidate.revision = format!("{}+{revision_suffix}", base.revision);
        }
        candidate.validate()?;
        *self.snapshot.write().await = candidate.clone();
        Ok(candidate)
    }

    /// Canonical id of an enabled or disabled catalog model, by id or alias.
    pub async fn canonical_id(&self, id_or_alias: &str) -> Result<String> {
        let canonical = canonical_catalog_id(id_or_alias.trim());
        self.snapshot
            .read()
            .await
            .models
            .iter()
            .find(|model| {
                model.id == canonical || model.aliases.iter().any(|alias| alias == &canonical)
            })
            .map(|model| model.id.clone())
            .ok_or(CatalogError::NotFound(canonical))
    }

    pub async fn snapshot(&self) -> CatalogSnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn resolve(&self, id_or_alias: &str) -> Result<CatalogModel> {
        let canonical = canonical_catalog_id(id_or_alias);
        self.snapshot
            .read()
            .await
            .models
            .iter()
            .find(|model| {
                model.enabled
                    && (model.id == canonical
                        || model.aliases.iter().any(|alias| alias == &canonical))
            })
            .cloned()
            .ok_or(CatalogError::NotFound(canonical))
    }

    pub async fn quote(
        &self,
        id_or_alias: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<PriceQuote> {
        let snapshot = self.snapshot.read().await;
        let canonical = canonical_catalog_id(id_or_alias);
        let model = snapshot
            .models
            .iter()
            .find(|model| {
                model.enabled
                    && (model.id == canonical
                        || model.aliases.iter().any(|alias| alias == &canonical))
            })
            .ok_or(CatalogError::NotFound(canonical))?;
        Ok(PriceQuote {
            model: model.id.clone(),
            input_tokens,
            output_tokens,
            amount_micros: model.pricing.quote(input_tokens, output_tokens)?,
            pricing: model.pricing,
            catalog_revision: snapshot.revision.clone(),
        })
    }

    pub async fn billing_price_table(
        &self,
        fallback: relay_billing::ModelPrice,
    ) -> relay_billing::PriceTable {
        let mut table = relay_billing::PriceTable::new(fallback);
        for model in self
            .snapshot
            .read()
            .await
            .models
            .iter()
            .filter(|model| model.enabled)
        {
            table.insert(
                relay_billing::ModelId::from_tensorzero_selector(&model.id),
                relay_billing::ModelPrice::new(
                    model.pricing.input_micro_usd_per_million,
                    model.pricing.output_micro_usd_per_million,
                ),
            );
        }
        table
    }

    pub async fn providers(&self) -> HashMap<String, usize> {
        let mut providers = HashMap::new();
        for model in self
            .snapshot
            .read()
            .await
            .models
            .iter()
            .filter(|model| model.enabled)
        {
            *providers.entry(model.provider.clone()).or_insert(0) += 1;
        }
        providers
    }

    /// Fail closed when an operator-configured TensorZero route is callable but
    /// absent from the effective priced catalog. Direct provider/model
    /// selectors can use the embedded provider catalog; named models and
    /// functions cannot be priced safely from their public names.
    pub async fn validate_configured_routes(
        &self,
        routes: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        let snapshot = self.snapshot.read().await;
        let enabled: std::collections::HashSet<String> = snapshot
            .models
            .iter()
            .filter(|model| model.enabled)
            .flat_map(|model| std::iter::once(&model.id).chain(model.aliases.iter()))
            .map(|identity| canonical_catalog_id(identity))
            .collect();
        let mut missing: Vec<String> = routes
            .into_iter()
            .map(|route| canonical_catalog_id(&route))
            .filter(|route| !enabled.contains(route))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CatalogError::MissingConfiguredRoutes(missing.join(", ")))
        }
    }
}

/// Parse the checked-in `data/catalog.json`.
pub fn bundled_snapshot() -> Result<CatalogSnapshot> {
    let snapshot: CatalogSnapshot = serde_json::from_str(crate::BUNDLED_CATALOG_JSON)
        .map_err(|error| CatalogError::InvalidJson(error.to_string()))?;
    snapshot.validate()?;
    Ok(snapshot)
}

fn builtin_model(id: &str, input: i64, output: i64) -> CatalogModel {
    let function_name = id.strip_prefix("tensorzero::function_name::");
    let configured_model_name = id.strip_prefix("tensorzero::model_name::");
    let provider = function_name.or(configured_model_name).map_or_else(
        || {
            id.split_once('/')
                .map_or("unknown", |(provider, _)| provider)
        },
        |_| "tensorzero",
    );
    let display_name = function_name
        .or(configured_model_name)
        .or_else(|| id.rsplit_once('/').map(|(_, model)| model))
        .unwrap_or(id)
        .replace(['-', '.'], " ");
    CatalogModel {
        id: id.into(),
        provider: provider.into(),
        display_name,
        aliases: Vec::new(),
        capabilities: ModelCapabilities::CHAT,
        pricing: ModelPrice::new(input, output, PricingSource::ProviderList),
        context_window_tokens: None,
        max_output_tokens: None,
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticCatalogSource;

    #[tokio::test]
    async fn builtin_catalog_resolves_selectors_and_quotes_exactly() {
        let service = CatalogService::builtin(123).await.unwrap();
        let model = service
            .resolve("tensorzero::model_name::openai::gpt-4o")
            .await
            .unwrap();
        assert_eq!(model.id, "openai/gpt-4o");
        let quote = service.quote(&model.id, 1_000, 500).await.unwrap();
        assert_eq!(quote.amount_micros.get(), 7_500);
    }

    #[test]
    fn bundled_catalog_matches_the_billing_price_table_exactly() {
        let bundled = bundled_snapshot().unwrap();
        let bundled_prices: HashMap<&str, (i64, i64)> = bundled
            .models
            .iter()
            .map(|model| {
                assert_eq!(model.pricing.source, PricingSource::ProviderList);
                (
                    model.id.as_str(),
                    (
                        model.pricing.input_micro_usd_per_million,
                        model.pricing.output_micro_usd_per_million,
                    ),
                )
            })
            .collect();
        assert_eq!(bundled_prices.len(), relay_billing::BUILTIN_PRICES.len());
        for (id, input, output) in relay_billing::BUILTIN_PRICES {
            assert_eq!(
                bundled_prices.get(id),
                Some(&(*input, *output)),
                "data/catalog.json and BUILTIN_PRICES disagree on {id}"
            );
        }
    }

    #[tokio::test]
    async fn builtin_rows_carry_bundled_capabilities() {
        let service = CatalogService::builtin(1).await.unwrap();
        let opus = service.resolve("anthropic/claude-opus-5.5").await.unwrap();
        assert!(opus.capabilities.vision);
        assert!(opus.context_window_tokens.is_some());
        let groq = service
            .resolve("tensorzero::model_name::groq::openai/gpt-oss-120b")
            .await
            .unwrap();
        assert_eq!(groq.provider, "groq");
    }

    #[tokio::test]
    async fn publish_rejects_invalid_candidates() {
        let service = CatalogService::builtin(1).await.unwrap();
        let mut bad = service.snapshot().await;
        bad.revision.clear();
        assert_eq!(service.publish(bad).await, Err(CatalogError::InvalidRevision));
        assert!(!service.snapshot().await.revision.is_empty());
    }

    #[tokio::test]
    async fn operator_price_edits_overlay_the_base_snapshot() {
        let base = CatalogSnapshot::new("base", 1, vec![builtin_model("acme/model", 1, 1)]);
        let service = CatalogService::initialize(Arc::new(StaticCatalogSource::new(base.clone())))
            .await
            .unwrap();
        let applied = service
            .apply_operator_prices(&base, [("acme/model".to_string(), 7, 9)], "ops-1")
            .await
            .unwrap();
        assert_eq!(applied.revision, "base+ops-1");
        let quote = service.quote("acme/model", 1_000_000, 0).await.unwrap();
        assert_eq!(quote.amount_micros.get(), 7);
        assert_eq!(quote.pricing.source, PricingSource::OperatorOverride);

        let missing = service
            .apply_operator_prices(&base, [("acme/other".to_string(), 1, 1)], "ops-2")
            .await
            .unwrap_err();
        assert_eq!(missing, CatalogError::NotFound("acme/other".into()));
        // The failed edit left the last-good snapshot in place.
        assert_eq!(service.snapshot().await.revision, "base+ops-1");

        service.apply_operator_prices(&base, [], "").await.unwrap();
        assert_eq!(service.snapshot().await.revision, "base");
    }

    #[tokio::test]
    async fn invalid_refresh_preserves_last_good_snapshot() {
        let valid = CatalogSnapshot::new("good", 1, vec![builtin_model("acme/model", 1, 1)]);
        let service = CatalogService::initialize(Arc::new(StaticCatalogSource::new(valid)))
            .await
            .unwrap();
        assert_eq!(service.snapshot().await.revision, "good");
    }

    #[tokio::test]
    async fn legacy_operator_overlay_is_visible_and_used_for_billing() {
        let config = relay_billing::catalog_config::parse(
            r#"{
                "models": [
                    {
                        "model": "tensorzero::model_name::openai::gpt-4o",
                        "input_micro_usd_per_million": 1000000,
                        "output_micro_usd_per_million": 2000000
                    },
                    {
                        "model": "acme/new-model",
                        "input_micro_usd_per_million": 3000000,
                        "output_micro_usd_per_million": 4000000
                    },
                    {
                        "model": "tensorzero::function_name::write_copy",
                        "input_micro_usd_per_million": 5000000,
                        "output_micro_usd_per_million": 6000000
                    },
                    {
                        "model": "tensorzero::model_name::production-chat",
                        "input_micro_usd_per_million": 7000000,
                        "output_micro_usd_per_million": 8000000
                    }
                ]
            }"#,
        )
        .unwrap();
        let service = CatalogService::builtin_with_price_overrides(123, &config)
            .await
            .unwrap();
        let quote = service.quote("openai/gpt-4o", 1_000, 500).await.unwrap();
        assert_eq!(quote.amount_micros.get(), 2_000);
        assert_eq!(quote.pricing.source, PricingSource::OperatorOverride);
        assert_eq!(
            service
                .resolve("acme/new-model")
                .await
                .unwrap()
                .pricing
                .source,
            PricingSource::OperatorOverride
        );
        let function = service
            .resolve("tensorzero::function_name::write_copy::variant_name::quality")
            .await
            .unwrap();
        assert_eq!(function.provider, "tensorzero");
        assert_eq!(function.id, "tensorzero::function_name::write_copy");
        assert_eq!(
            service
                .quote(&function.id, 1_000, 500)
                .await
                .unwrap()
                .amount_micros
                .get(),
            8_000
        );
        let configured_model = service
            .resolve("tensorzero::model_name::production-chat")
            .await
            .unwrap();
        assert_eq!(configured_model.provider, "tensorzero");
        assert_eq!(
            service
                .quote(&configured_model.id, 1_000, 500)
                .await
                .unwrap()
                .amount_micros
                .get(),
            11_000
        );

        let table = service
            .billing_price_table(relay_billing::ModelPrice::new(9, 9))
            .await;
        assert_eq!(
            table.price_for(&relay_billing::ModelId::namespace("openai/gpt-4o")),
            relay_billing::ModelPrice::new(1_000_000, 2_000_000)
        );
        assert_eq!(
            table.price_for(&relay_billing::ModelId::from_tensorzero_selector(
                "tensorzero::function_name::write_copy"
            )),
            relay_billing::ModelPrice::new(5_000_000, 6_000_000)
        );
        assert_eq!(
            table.price_for(&relay_billing::ModelId::from_tensorzero_selector(
                "tensorzero::model_name::production-chat"
            )),
            relay_billing::ModelPrice::new(7_000_000, 8_000_000)
        );
    }

    #[tokio::test]
    async fn configured_route_coverage_requires_every_enabled_retail_identity() {
        let service =
            CatalogService::initialize(Arc::new(StaticCatalogSource::new(CatalogSnapshot::new(
                "operator",
                1,
                vec![
                    builtin_model("tensorzero::model_name::production-chat", 1, 1),
                    builtin_model("tensorzero::function_name::summarize", 1, 1),
                ],
            ))))
            .await
            .unwrap();

        service
            .validate_configured_routes([
                "tensorzero::model_name::production-chat".into(),
                "tensorzero::function_name::summarize::variant_name::fast".into(),
            ])
            .await
            .unwrap();

        let error = service
            .validate_configured_routes([
                "tensorzero::function_name::missing".into(),
                "tensorzero::model_name::also-missing".into(),
                "tensorzero::function_name::missing".into(),
            ])
            .await
            .unwrap_err();
        assert_eq!(
            error,
            CatalogError::MissingConfiguredRoutes(
                "tensorzero::function_name::missing, tensorzero::model_name::also-missing".into()
            )
        );
    }
}

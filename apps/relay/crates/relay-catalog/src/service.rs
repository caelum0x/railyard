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

    pub async fn builtin(generated_at_ms: i64) -> Result<Self> {
        let models = relay_billing::BUILTIN_PRICES
            .iter()
            .map(|(id, input, output)| builtin_model(id, *input, *output))
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

    /// Atomically refresh. An invalid source result leaves the last-good
    /// snapshot serving traffic.
    pub async fn refresh(&self) -> Result<CatalogSnapshot> {
        let candidate = self.source.load().await?;
        candidate.validate()?;
        *self.snapshot.write().await = candidate.clone();
        Ok(candidate)
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

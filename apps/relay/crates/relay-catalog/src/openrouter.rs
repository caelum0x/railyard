//! Refresh list prices from OpenRouter's public model feed.
//!
//! `GET https://openrouter.ai/api/v1/models` is public (no key) and lists every
//! routable model with its per-token USD price as a decimal string. This module
//! turns that feed into a [`CatalogSnapshot`] by merging it onto a base
//! snapshot (the deployed catalog):
//!
//! * only rows priced from a provider list ([`PricingSource::ProviderList`])
//!   are repriced; operator overrides, contract prices, conservative fallbacks
//!   and TensorZero function / named-model routes are never touched, so an
//!   operator's price always sits above the feed;
//! * new feed models are added only when [`FeedOptions::add_new_models`] is
//!   set (default off: the gateway should not advertise routes it was never
//!   configured to serve);
//! * the merge fails closed: a feed that does not parse, is too small, or has
//!   too many unusable rows is an error and the caller keeps its last-good
//!   snapshot. A paid model whose feed price suddenly reads `0/0` keeps its
//!   previous price (only `:free` ids may be free), and negative / variable
//!   prices (`"-1"`, router meta-models) are skipped.
//!
//! Money stays integer: the decimal USD-per-token string is scaled to micro-USD
//! per million tokens exactly (`"0.000003"` → `3_000_000`), rounding any
//! sub-micro remainder UP so a model is never under-billed.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::model::canonical_catalog_id;
use crate::{
    CatalogError, CatalogModel, CatalogSnapshot, CatalogSource, ModelCapabilities, ModelPrice,
    PricingSource, Result,
};

/// OpenRouter's public model list.
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Largest feed body accepted (the real feed is well under 2 MiB).
pub const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

/// Revision marker appended to the base revision on every feed merge.
const REVISION_MARKER: &str = "+openrouter-";

/// How a feed is merged onto the base snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedOptions {
    /// Append feed models the base does not list (default `false`).
    pub add_new_models: bool,
    /// Minimum number of usable priced rows for the feed to be trusted.
    pub min_priced_models: usize,
    /// Maximum share (percent) of unusable rows before the feed is rejected.
    pub max_unusable_percent: u8,
}

impl Default for FeedOptions {
    fn default() -> Self {
        Self {
            add_new_models: false,
            min_priced_models: 50,
            max_unusable_percent: 25,
        }
    }
}

/// What a merge changed (for logs and the operator console).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Base rows whose price or limits changed.
    pub repriced: Vec<String>,
    /// Feed models appended to the catalog.
    pub added: Vec<String>,
    /// Paid rows the feed reported as free; their previous price was kept.
    pub suspicious_zero: Vec<String>,
    /// Feed rows skipped as unusable (bad or variable price, non-text output).
    pub skipped: usize,
}

#[derive(Debug, Deserialize)]
struct Feed {
    data: Vec<FeedModel>,
}

#[derive(Debug, Deserialize)]
struct FeedModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    pricing: Option<FeedPricing>,
    #[serde(default)]
    architecture: Option<FeedArchitecture>,
    #[serde(default)]
    top_provider: Option<FeedTopProvider>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct FeedPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeedArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FeedTopProvider {
    #[serde(default)]
    max_completion_tokens: Option<u64>,
}

/// One usable feed row, already converted to integer micro-USD per 1M tokens.
#[derive(Debug, Clone)]
struct PricedFeedModel {
    id: String,
    name: Option<String>,
    input: i64,
    output: i64,
    context_window_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    capabilities: ModelCapabilities,
}

/// Convert a decimal USD-per-token string to micro-USD per million tokens
/// (`× 10^12`), exactly and rounding up. `None` for negative, malformed or
/// overflowing values.
pub fn usd_per_token_to_micro_per_million(raw: &str) -> Option<i64> {
    const SCALE_DIGITS: usize = 12;
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('-') || raw.starts_with('+') {
        return None;
    }
    let (int_part, frac_part) = raw.split_once('.').unwrap_or((raw, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (kept, rest) = if frac_part.len() > SCALE_DIGITS {
        frac_part.split_at(SCALE_DIGITS)
    } else {
        (frac_part, "")
    };
    let mut digits = String::with_capacity(int_part.len() + SCALE_DIGITS);
    digits.push_str(int_part);
    digits.push_str(kept);
    digits.extend(std::iter::repeat_n('0', SCALE_DIGITS - kept.len()));
    let trimmed = digits.trim_start_matches('0');
    let mut value: i64 = if trimmed.is_empty() {
        0
    } else {
        trimmed.parse().ok()?
    };
    if rest.bytes().any(|b| b != b'0') {
        value = value.checked_add(1)?;
    }
    Some(value)
}

fn parse_feed(feed_json: &str) -> Result<(Vec<PricedFeedModel>, usize)> {
    let feed: Feed = serde_json::from_str(feed_json)
        .map_err(|error| CatalogError::InvalidJson(format!("openrouter feed: {error}")))?;
    let total = feed.data.len();
    let mut usable = Vec::with_capacity(total);
    for model in feed.data {
        if let Some(priced) = priced(model) {
            usable.push(priced);
        }
    }
    Ok((usable, total))
}

fn priced(model: FeedModel) -> Option<PricedFeedModel> {
    let id = model.id.trim().to_string();
    if id.is_empty() || !id.contains('/') || id.chars().any(char::is_whitespace) {
        return None;
    }
    // Batch variants are priced for the async batch API, not live inference.
    if id.ends_with(":batch") {
        return None;
    }
    let architecture = model.architecture?;
    if architecture.output_modalities != ["text"] {
        return None;
    }
    let pricing = model.pricing?;
    let input = usd_per_token_to_micro_per_million(pricing.prompt.as_deref()?)?;
    let output = usd_per_token_to_micro_per_million(pricing.completion.as_deref()?)?;
    let parameters: HashSet<String> = model
        .supported_parameters
        .unwrap_or_default()
        .into_iter()
        .collect();
    Some(PricedFeedModel {
        id,
        name: model.name.filter(|name| !name.trim().is_empty()),
        input,
        output,
        context_window_tokens: model.context_length.filter(|n| *n > 0),
        max_output_tokens: model
            .top_provider
            .and_then(|provider| provider.max_completion_tokens)
            .filter(|n| *n > 0),
        capabilities: ModelCapabilities {
            chat: true,
            embeddings: false,
            tools: parameters.contains("tools"),
            vision: architecture.input_modalities.iter().any(|m| m == "image"),
            json_schema: parameters.contains("structured_outputs")
                || parameters.contains("response_format"),
            streaming: true,
            batch: false,
        },
    })
}

fn base_revision(revision: &str) -> &str {
    revision
        .find(REVISION_MARKER)
        .map_or(revision, |index| &revision[..index])
}

/// Merge an OpenRouter feed onto `base` (see the module docs). Returns the
/// validated candidate snapshot and a report of what changed.
pub fn merge_openrouter_feed(
    base: &CatalogSnapshot,
    feed_json: &str,
    now_ms: i64,
    options: FeedOptions,
) -> Result<(CatalogSnapshot, MergeReport)> {
    let (usable, total) = parse_feed(feed_json)?;
    if usable.len() < options.min_priced_models {
        return Err(CatalogError::Source(format!(
            "openrouter feed has {} usable priced models; at least {} required",
            usable.len(),
            options.min_priced_models
        )));
    }
    let unusable = total - usable.len();
    if unusable * 100 > total * usize::from(options.max_unusable_percent) {
        return Err(CatalogError::Source(format!(
            "openrouter feed rejected: {unusable} of {total} rows are unusable"
        )));
    }

    let mut report = MergeReport {
        skipped: unusable,
        ..MergeReport::default()
    };
    let by_id: HashMap<String, &PricedFeedModel> = usable
        .iter()
        .map(|model| (canonical_catalog_id(&model.id), model))
        .collect();

    let mut candidate = base.clone();
    let mut identities: HashSet<String> = HashSet::new();
    for model in &mut candidate.models {
        identities.insert(canonical_catalog_id(&model.id));
        identities.extend(model.aliases.iter().map(|alias| canonical_catalog_id(alias)));
        if model.pricing.source != PricingSource::ProviderList {
            continue;
        }
        let Some(feed) = by_id.get(&canonical_catalog_id(&model.id)) else {
            continue;
        };
        let was_paid = model.pricing.input_micro_usd_per_million > 0
            || model.pricing.output_micro_usd_per_million > 0;
        let now_free = feed.input == 0 && feed.output == 0;
        if was_paid && now_free && !model.id.ends_with(":free") {
            report.suspicious_zero.push(model.id.clone());
            continue;
        }
        let pricing = ModelPrice::new(feed.input, feed.output, PricingSource::ProviderList);
        let context = feed.context_window_tokens.or(model.context_window_tokens);
        let max_output = feed.max_output_tokens.or(model.max_output_tokens);
        if pricing != model.pricing
            || context != model.context_window_tokens
            || max_output != model.max_output_tokens
        {
            model.pricing = pricing;
            model.context_window_tokens = context;
            model.max_output_tokens = max_output;
            report.repriced.push(model.id.clone());
        }
    }

    if options.add_new_models {
        let mut additions: Vec<&PricedFeedModel> = usable
            .iter()
            .filter(|model| !identities.contains(&canonical_catalog_id(&model.id)))
            .collect();
        additions.sort_by(|a, b| a.id.cmp(&b.id));
        for feed in additions {
            let id = canonical_catalog_id(&feed.id);
            if !identities.insert(id.clone()) {
                continue;
            }
            let provider = id.split_once('/').map_or("", |(provider, _)| provider);
            candidate.models.push(CatalogModel {
                display_name: feed.name.clone().unwrap_or_else(|| id.clone()),
                provider: provider.to_string(),
                aliases: Vec::new(),
                capabilities: feed.capabilities,
                pricing: ModelPrice::new(feed.input, feed.output, PricingSource::ProviderList),
                context_window_tokens: feed.context_window_tokens,
                max_output_tokens: feed.max_output_tokens,
                enabled: true,
                id: id.clone(),
            });
            report.added.push(id);
        }
    }

    candidate.revision = format!("{}{REVISION_MARKER}{now_ms}", base_revision(&base.revision));
    candidate.generated_at_ms = now_ms;
    candidate.validate()?;
    Ok((candidate, report))
}

/// A [`CatalogSource`] that fetches OpenRouter's public model list over HTTPS
/// and merges it onto a base snapshot.
#[derive(Debug, Clone)]
pub struct HttpOpenRouterCatalogSource {
    client: reqwest::Client,
    url: String,
    base: CatalogSnapshot,
    options: FeedOptions,
}

impl HttpOpenRouterCatalogSource {
    /// `url` must be `https://` (plain `http://` is accepted only for a
    /// loopback host, e.g. a local mirror in tests).
    pub fn new(url: impl Into<String>, base: CatalogSnapshot, options: FeedOptions) -> Result<Self> {
        let url = url.into();
        let trimmed = url.trim();
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix));
        if !trimmed.starts_with("https://") && !loopback {
            return Err(CatalogError::Source(format!(
                "catalog refresh URL must use https: {trimmed}"
            )));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("railyard-relay-catalog")
            .build()
            .map_err(|error| CatalogError::Source(error.to_string()))?;
        Ok(Self {
            client,
            url: trimmed.to_string(),
            base,
            options,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Fetch the raw feed body (status and size checked).
    pub async fn fetch_feed(&self) -> Result<String> {
        let response = self
            .client
            .get(&self.url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| CatalogError::Source(format!("openrouter feed request: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(CatalogError::Source(format!(
                "openrouter feed returned HTTP {status}"
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FEED_BYTES as u64)
        {
            return Err(CatalogError::Source("openrouter feed is too large".into()));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CatalogError::Source(format!("openrouter feed body: {error}")))?;
        if bytes.len() > MAX_FEED_BYTES {
            return Err(CatalogError::Source("openrouter feed is too large".into()));
        }
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CatalogError::InvalidJson("openrouter feed is not UTF-8".into()))
    }

    /// Fetch and merge onto `base` (typically the currently deployed base).
    pub async fn merge_onto(
        &self,
        base: &CatalogSnapshot,
        now_ms: i64,
    ) -> Result<(CatalogSnapshot, MergeReport)> {
        let feed = self.fetch_feed().await?;
        merge_openrouter_feed(base, &feed, now_ms, self.options)
    }
}

#[async_trait]
impl CatalogSource for HttpOpenRouterCatalogSource {
    async fn load(&self) -> Result<CatalogSnapshot> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
        self.merge_onto(&self.base, now_ms)
            .await
            .map(|(snapshot, _report)| snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/openrouter_models.json");

    fn lenient() -> FeedOptions {
        FeedOptions {
            min_priced_models: 1,
            max_unusable_percent: 50,
            ..FeedOptions::default()
        }
    }

    fn list_model(id: &str, input: i64, output: i64, source: PricingSource) -> CatalogModel {
        CatalogModel {
            id: id.into(),
            provider: id.split_once('/').map_or("tensorzero", |(p, _)| p).into(),
            display_name: id.into(),
            aliases: Vec::new(),
            capabilities: ModelCapabilities::CHAT,
            pricing: ModelPrice::new(input, output, source),
            context_window_tokens: None,
            max_output_tokens: None,
            enabled: true,
        }
    }

    #[test]
    fn decimal_prices_scale_exactly_and_round_up() {
        assert_eq!(usd_per_token_to_micro_per_million("0.000003"), Some(3_000_000));
        assert_eq!(usd_per_token_to_micro_per_million("0.00000015"), Some(150_000));
        assert_eq!(usd_per_token_to_micro_per_million("0"), Some(0));
        assert_eq!(usd_per_token_to_micro_per_million("0.0000000000001"), Some(1));
        assert_eq!(usd_per_token_to_micro_per_million("1"), Some(1_000_000_000_000));
        assert_eq!(usd_per_token_to_micro_per_million("-1"), None);
        assert_eq!(usd_per_token_to_micro_per_million("abc"), None);
        assert_eq!(usd_per_token_to_micro_per_million("1e-6"), None);
        assert_eq!(usd_per_token_to_micro_per_million(""), None);
    }

    #[test]
    fn feed_reprices_list_rows_but_never_operator_overrides() {
        let base = CatalogSnapshot::new(
            "deployed-v1",
            1,
            vec![
                list_model("openai/gpt-4o", 1, 1, PricingSource::ProviderList),
                list_model("openai/gpt-4o-mini", 9, 9, PricingSource::OperatorOverride),
                list_model("acme/private", 5, 5, PricingSource::ProviderList),
            ],
        );
        let (merged, report) = merge_openrouter_feed(&base, FIXTURE, 42, lenient()).unwrap();
        let get = |id: &str| merged.models.iter().find(|m| m.id == id).unwrap().clone();
        assert_eq!(
            get("openai/gpt-4o").pricing,
            ModelPrice::new(2_500_000, 10_000_000, PricingSource::ProviderList)
        );
        assert_eq!(get("openai/gpt-4o").context_window_tokens, Some(128_000));
        // Operator override wins over the feed.
        assert_eq!(
            get("openai/gpt-4o-mini").pricing,
            ModelPrice::new(9, 9, PricingSource::OperatorOverride)
        );
        // A row the feed does not list is left alone.
        assert_eq!(get("acme/private").pricing.input_micro_usd_per_million, 5);
        assert_eq!(report.repriced, vec!["openai/gpt-4o".to_string()]);
        assert!(report.added.is_empty());
        assert_eq!(merged.revision, "deployed-v1+openrouter-42");
        assert_eq!(merged.generated_at_ms, 42);

        // Re-merging does not stack revision suffixes.
        let (again, _) = merge_openrouter_feed(&merged, FIXTURE, 43, lenient()).unwrap();
        assert_eq!(again.revision, "deployed-v1+openrouter-43");
    }

    #[test]
    fn add_new_models_appends_priced_text_models_only() {
        let base = CatalogSnapshot::new(
            "deployed-v1",
            1,
            vec![list_model("openai/gpt-4o", 1, 1, PricingSource::ProviderList)],
        );
        let options = FeedOptions {
            add_new_models: true,
            ..lenient()
        };
        let (merged, report) = merge_openrouter_feed(&base, FIXTURE, 7, options).unwrap();
        assert!(report.added.contains(&"anthropic/claude-opus-5.5".to_string()));
        assert!(report.added.contains(&"google/gemma-4-31b-it:free".to_string()));
        // Batch variants are never imported as live routes.
        assert!(!merged.models.iter().any(|m| m.id.ends_with(":batch")));
        let opus = merged
            .models
            .iter()
            .find(|m| m.id == "anthropic/claude-opus-5.5")
            .unwrap();
        assert_eq!(opus.pricing.input_micro_usd_per_million, 4_000_000);
        assert_eq!(opus.pricing.output_micro_usd_per_million, 20_000_000);
        assert!(opus.capabilities.vision);
        let free = merged
            .models
            .iter()
            .find(|m| m.id == "google/gemma-4-31b-it:free")
            .unwrap();
        assert_eq!(free.pricing.input_micro_usd_per_million, 0);
    }

    #[test]
    fn a_paid_model_reading_free_keeps_its_price() {
        let feed = r#"{"data":[{"id":"openai/gpt-4o","architecture":{"output_modalities":["text"]},"pricing":{"prompt":"0","completion":"0"}}]}"#;
        let base = CatalogSnapshot::new(
            "b",
            1,
            vec![list_model("openai/gpt-4o", 2_500_000, 10_000_000, PricingSource::ProviderList)],
        );
        let (merged, report) = merge_openrouter_feed(&base, feed, 5, lenient()).unwrap();
        assert_eq!(merged.models[0].pricing.input_micro_usd_per_million, 2_500_000);
        assert_eq!(report.suspicious_zero, vec!["openai/gpt-4o".to_string()]);
    }

    #[test]
    fn bad_feeds_fail_closed() {
        let base = CatalogSnapshot::new(
            "b",
            1,
            vec![list_model("openai/gpt-4o", 1, 1, PricingSource::ProviderList)],
        );
        assert!(matches!(
            merge_openrouter_feed(&base, "not json", 1, lenient()),
            Err(CatalogError::InvalidJson(_))
        ));
        assert!(matches!(
            merge_openrouter_feed(&base, r#"{"data":[]}"#, 1, lenient()),
            Err(CatalogError::Source(_))
        ));
        // The real default threshold rejects a tiny (truncated) feed.
        assert!(matches!(
            merge_openrouter_feed(&base, FIXTURE, 1, FeedOptions::default()),
            Err(CatalogError::Source(_))
        ));
        // Mostly-garbage feed: too many unusable rows.
        let garbage = r#"{"data":[
            {"id":"a/x","architecture":{"output_modalities":["text"]},"pricing":{"prompt":"-1","completion":"-1"}},
            {"id":"a/y","architecture":{"output_modalities":["image"]},"pricing":{"prompt":"0.1","completion":"0.1"}},
            {"id":"a/z","architecture":{"output_modalities":["text"]},"pricing":{"prompt":"0.000001","completion":"0.000002"}}
        ]}"#;
        assert!(matches!(
            merge_openrouter_feed(&base, garbage, 1, lenient()),
            Err(CatalogError::Source(_))
        ));
    }

    #[test]
    fn source_requires_https() {
        let base = CatalogSnapshot::new("b", 1, Vec::new());
        assert!(HttpOpenRouterCatalogSource::new(
            "http://example.com/models",
            base.clone(),
            FeedOptions::default()
        )
        .is_err());
        assert!(HttpOpenRouterCatalogSource::new(
            OPENROUTER_MODELS_URL,
            base.clone(),
            FeedOptions::default()
        )
        .is_ok());
        assert!(HttpOpenRouterCatalogSource::new(
            "http://127.0.0.1:9/models",
            base,
            FeedOptions::default()
        )
        .is_ok());
    }
}

//! Read-only price cross-check against LiteLLM's MIT price list (RY-RL2).
//!
//! `BerriAI/litellm` publishes `model_prices_and_context_window.json` (MIT, outside
//! its `enterprise/` folder). Relay does NOT run LiteLLM (ADR RY-0001); it only
//! reads this file as a second opinion on its own catalog prices.
//!
//! The cross-check is a REPORT. It takes the catalog by shared reference and
//! returns a [`PriceCrossCheck`]; it never changes a price, never publishes a
//! snapshot, and an operator price always wins. A difference above the
//! tolerance (default 5%) is flagged for the operator to look at.
//!
//! Lookup per catalog row:
//! * a provider/model row (`anthropic/claude-sonnet-4.5`) is compared with
//!   LiteLLM's OpenRouter entry (`openrouter/<id>`), then the provider's own
//!   entry (`groq/<model>`, `claude-sonnet-4-5`, `gpt-4o`);
//! * a named route (`tensorzero::model_name::claude-haiku`) is compared with the
//!   list price of its FIRST leg, taken from the route map the caller passes
//!   (the provider canary's `routes.json`). A named route priced as a retail
//!   markup shows up as a flagged difference by design;
//! * functions (`tensorzero::function_name::*`) have no single model and are skipped.
//!
//! LiteLLM stores USD per token as JSON numbers. They are converted to the
//! catalog's integer micro-USD per million with the same exact decimal scaler
//! as the OpenRouter feed; floats never reach a money path.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openrouter::usd_per_token_to_micro_per_million;
use crate::{CatalogError, CatalogSnapshot, Result};

/// The raw price file on the LiteLLM main branch.
pub const LITELLM_PRICES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// Flag differences above 5% (basis points).
pub const DEFAULT_TOLERANCE_BPS: u32 = 500;
/// Largest price file accepted (the real one is about 3 MiB).
pub const MAX_LITELLM_BYTES: usize = 32 * 1024 * 1024;

const MODEL_PREFIX: &str = "tensorzero::model_name::";
const FUNCTION_PREFIX: &str = "tensorzero::function_name::";

/// One LiteLLM price, in the catalog's unit (micro-USD per 1M tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiteLlmPrice {
    pub input_micro_usd_per_million: i64,
    pub output_micro_usd_per_million: i64,
}

/// LiteLLM prices keyed by LiteLLM model key.
#[derive(Debug, Clone, Default)]
pub struct LiteLlmPrices {
    prices: HashMap<String, LiteLlmPrice>,
}

impl LiteLlmPrices {
    pub fn len(&self) -> usize {
        self.prices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<LiteLlmPrice> {
        self.prices.get(key).copied()
    }
}

/// One provider leg of a named route: `{"provider": "anthropic", "model": "claude-haiku-4-5"}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RouteLeg {
    pub provider: String,
    pub model: String,
}

#[derive(Deserialize)]
struct RoutesFile {
    routes: Vec<RouteEntry>,
}

#[derive(Deserialize)]
struct RouteEntry {
    name: String,
    legs: Vec<RouteLeg>,
}

/// Parse the provider canary's `routes.json` into `route name -> legs`.
pub fn parse_route_legs(json: &str) -> Result<BTreeMap<String, Vec<RouteLeg>>> {
    let file: RoutesFile =
        serde_json::from_str(json).map_err(|error| CatalogError::InvalidJson(error.to_string()))?;
    Ok(file.routes.into_iter().map(|r| (r.name, r.legs)).collect())
}

/// Parse LiteLLM's price file. Rows without both per-token prices are skipped
/// (images, audio, rerank, the `sample_spec` row).
pub fn parse_litellm_prices(json: &str) -> Result<LiteLlmPrices> {
    let root: Value =
        serde_json::from_str(json).map_err(|error| CatalogError::InvalidJson(error.to_string()))?;
    let Value::Object(rows) = root else {
        return Err(CatalogError::InvalidJson(
            "LiteLLM price file is not an object".into(),
        ));
    };
    let mut prices = HashMap::new();
    for (key, row) in rows {
        let (Some(input), Some(output)) = (
            row.get("input_cost_per_token").and_then(scale),
            row.get("output_cost_per_token").and_then(scale),
        ) else {
            continue;
        };
        prices.insert(
            key,
            LiteLlmPrice {
                input_micro_usd_per_million: input,
                output_micro_usd_per_million: output,
            },
        );
    }
    if prices.is_empty() {
        return Err(CatalogError::InvalidJson(
            "LiteLLM price file has no priced rows".into(),
        ));
    }
    Ok(LiteLlmPrices { prices })
}

fn scale(value: &Value) -> Option<i64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    // f64 Display never uses an exponent, so this is a plain decimal string.
    usd_per_token_to_micro_per_million(&format!("{number}"))
}

/// Download the LiteLLM price file (https only, size-capped).
pub async fn fetch_litellm_prices(url: &str) -> Result<String> {
    let url = url.trim();
    let loopback = ["http://127.0.0.1", "http://localhost"]
        .iter()
        .any(|p| url.starts_with(p));
    if !url.starts_with("https://") && !loopback {
        return Err(CatalogError::Source(format!(
            "LiteLLM price URL must use https: {url}"
        )));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("railyard-relay-catalog")
        .build()
        .map_err(|error| CatalogError::Source(error.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| CatalogError::Source(format!("LiteLLM price request: {error}")))?;
    if !response.status().is_success() {
        return Err(CatalogError::Source(format!(
            "LiteLLM price file returned HTTP {}",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| CatalogError::Source(format!("LiteLLM price body: {error}")))?;
    if bytes.len() > MAX_LITELLM_BYTES {
        return Err(CatalogError::Source(
            "LiteLLM price file is too large".into(),
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| CatalogError::InvalidJson("LiteLLM price file is not UTF-8".into()))
}

/// What kind of catalog row was compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// A provider/model list price.
    Model,
    /// A named Relay route, compared with its first leg's list price.
    Route,
}

/// One catalog row whose price differs from LiteLLM by more than the tolerance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriceMismatch {
    pub model: String,
    pub kind: RowKind,
    pub litellm_key: String,
    pub ours_input_micro_usd_per_million: i64,
    pub ours_output_micro_usd_per_million: i64,
    pub litellm_input_micro_usd_per_million: i64,
    pub litellm_output_micro_usd_per_million: i64,
    /// Relative difference of the input rate, in basis points of LiteLLM's price.
    pub input_diff_bps: u64,
    pub output_diff_bps: u64,
}

/// The report. Changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PriceCrossCheck {
    pub catalog_revision: String,
    pub tolerance_bps: u32,
    /// Rows compared (found in LiteLLM).
    pub compared: usize,
    /// Compared rows within the tolerance.
    pub matched: usize,
    pub mismatches: Vec<PriceMismatch>,
    /// Rows with no LiteLLM entry under any candidate key.
    pub not_found: Vec<String>,
    /// Functions and named routes without a known first leg.
    pub skipped: Vec<String>,
}

/// LiteLLM keys to try for a provider/model catalog id, best first.
fn model_candidates(id: &str) -> Vec<String> {
    let mut out = vec![format!("openrouter/{id}"), id.to_string()];
    if let Some((provider, model)) = id.split_once('/') {
        out.extend(leg_candidates(provider, model));
    }
    out.dedup();
    out
}

/// LiteLLM keys to try for one provider leg (`provider_type`, provider model name).
fn leg_candidates(provider: &str, model: &str) -> Vec<String> {
    match provider {
        "openrouter" => vec![format!("openrouter/{model}")],
        "groq" => vec![format!("groq/{model}")],
        "anthropic" => vec![model.to_string(), model.replace('.', "-")],
        "openai" => vec![model.to_string(), format!("openai/{model}")],
        other => vec![format!("{other}/{model}"), model.to_string()],
    }
}

fn diff_bps(ours: i64, theirs: i64) -> u64 {
    let delta = (i128::from(ours) - i128::from(theirs)).unsigned_abs();
    if theirs == 0 {
        return if delta == 0 { 0 } else { u64::MAX };
    }
    u64::try_from(delta * 10_000 / i128::from(theirs).unsigned_abs()).unwrap_or(u64::MAX)
}

/// Compare every enabled catalog row with LiteLLM. Read-only.
pub fn cross_check(
    snapshot: &CatalogSnapshot,
    litellm: &LiteLlmPrices,
    routes: &BTreeMap<String, Vec<RouteLeg>>,
    tolerance_bps: u32,
) -> PriceCrossCheck {
    let mut report = PriceCrossCheck {
        catalog_revision: snapshot.revision.clone(),
        tolerance_bps,
        ..PriceCrossCheck::default()
    };
    for model in snapshot.models.iter().filter(|m| m.enabled) {
        let id = model.id.as_str();
        let (kind, candidates) = if id.starts_with(FUNCTION_PREFIX) {
            report.skipped.push(id.to_string());
            continue;
        } else if let Some(route) = id.strip_prefix(MODEL_PREFIX) {
            match routes.get(route).and_then(|legs| legs.first()) {
                Some(leg) => (RowKind::Route, leg_candidates(&leg.provider, &leg.model)),
                None => {
                    report.skipped.push(id.to_string());
                    continue;
                }
            }
        } else {
            (RowKind::Model, model_candidates(id))
        };
        let Some((key, price)) = candidates
            .into_iter()
            .find_map(|key| litellm.get(&key).map(|price| (key, price)))
        else {
            report.not_found.push(id.to_string());
            continue;
        };
        report.compared += 1;
        let ours_in = model.pricing.input_micro_usd_per_million;
        let ours_out = model.pricing.output_micro_usd_per_million;
        let input_diff = diff_bps(ours_in, price.input_micro_usd_per_million);
        let output_diff = diff_bps(ours_out, price.output_micro_usd_per_million);
        let limit = u64::from(tolerance_bps);
        if input_diff > limit || output_diff > limit {
            report.mismatches.push(PriceMismatch {
                model: id.to_string(),
                kind,
                litellm_key: key,
                ours_input_micro_usd_per_million: ours_in,
                ours_output_micro_usd_per_million: ours_out,
                litellm_input_micro_usd_per_million: price.input_micro_usd_per_million,
                litellm_output_micro_usd_per_million: price.output_micro_usd_per_million,
                input_diff_bps: input_diff,
                output_diff_bps: output_diff,
            });
        } else {
            report.matched += 1;
        }
    }
    report
}

/// Plain-text report for the operator.
pub fn render_text(report: &PriceCrossCheck) -> String {
    use std::fmt::Write as _;
    let usd = |micro: i64| format!("${}.{:06}", micro / 1_000_000, micro % 1_000_000);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "LiteLLM price cross-check of catalog {} (read-only; no price was changed)",
        report.catalog_revision
    );
    let _ = writeln!(
        out,
        "compared {}, within {}%: {}, flagged: {}, not in LiteLLM: {}, skipped: {}",
        report.compared,
        f64::from(report.tolerance_bps) / 100.0,
        report.matched,
        report.mismatches.len(),
        report.not_found.len(),
        report.skipped.len()
    );
    for m in &report.mismatches {
        let kind = match m.kind {
            RowKind::Model => "model",
            RowKind::Route => "route (retail vs first leg)",
        };
        let _ = writeln!(
            out,
            "FLAG {} [{}] ours in {} out {} per 1M; LiteLLM {} in {} out {} per 1M",
            m.model,
            kind,
            usd(m.ours_input_micro_usd_per_million),
            usd(m.ours_output_micro_usd_per_million),
            m.litellm_key,
            usd(m.litellm_input_micro_usd_per_million),
            usd(m.litellm_output_micro_usd_per_million),
        );
    }
    for id in &report.not_found {
        let _ = writeln!(out, "not in LiteLLM: {id}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogModel, ModelCapabilities, ModelPrice, PricingSource};

    const FIXTURE: &str = include_str!("../tests/fixtures/litellm_prices.json");

    fn row(id: &str, input: i64, output: i64) -> CatalogModel {
        let provider = if id.starts_with("tensorzero::") {
            "tensorzero"
        } else {
            id.split_once('/').map_or("unknown", |(p, _)| p)
        };
        CatalogModel {
            id: id.into(),
            provider: provider.into(),
            display_name: id.into(),
            aliases: Vec::new(),
            capabilities: ModelCapabilities::CHAT,
            pricing: ModelPrice::new(input, output, PricingSource::OperatorOverride),
            context_window_tokens: None,
            max_output_tokens: None,
            enabled: true,
        }
    }

    fn routes() -> BTreeMap<String, Vec<RouteLeg>> {
        let leg = |p: &str, m: &str| RouteLeg {
            provider: p.into(),
            model: m.into(),
        };
        BTreeMap::from([
            (
                "claude-haiku".into(),
                vec![
                    leg("anthropic", "claude-haiku-4-5"),
                    leg("openrouter", "anthropic/claude-haiku-4.5"),
                ],
            ),
            (
                "llama-3.3-70b-versatile".into(),
                vec![leg("groq", "openai/gpt-oss-120b")],
            ),
        ])
    }

    #[test]
    fn litellm_floats_scale_exactly_to_micro_per_million() {
        let prices = parse_litellm_prices(FIXTURE).unwrap();
        assert_eq!(
            prices.get("claude-haiku-4-5"),
            Some(LiteLlmPrice {
                input_micro_usd_per_million: 1_000_000,
                output_micro_usd_per_million: 5_000_000
            })
        );
        assert_eq!(
            prices
                .get("groq/openai/gpt-oss-20b")
                .unwrap()
                .input_micro_usd_per_million,
            75_000
        );
        // sample_spec and unpriced rows are skipped
        assert!(prices.get("sample_spec").is_none());
        assert!(prices.get("github_copilot/claude-haiku-4.5").is_none());
    }

    #[test]
    fn flags_only_differences_above_the_tolerance() {
        let snapshot = CatalogSnapshot::new(
            "r1",
            1,
            vec![
                row("anthropic/claude-haiku-4.5", 1_000_000, 5_000_000),
                row("anthropic/claude-sonnet-5", 2_080_000, 10_000_000), // +4%: ok
                row("openai/gpt-oss-20b", 75_000, 300_000), // LiteLLM OpenRouter row is cheaper
                row("tensorzero::model_name::claude-haiku", 1_000_000, 5_000_000),
                row(
                    "tensorzero::model_name::llama-3.3-70b-versatile",
                    590_000,
                    790_000,
                ),
                row("tensorzero::function_name::chat", 150_000, 600_000),
                row("tensorzero::model_name::unknown-route", 1, 1),
                row("acme/not-listed", 1, 1),
            ],
        );
        let prices = parse_litellm_prices(FIXTURE).unwrap();
        let report = cross_check(&snapshot, &prices, &routes(), DEFAULT_TOLERANCE_BPS);
        let flagged: Vec<_> = report.mismatches.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(
            flagged,
            [
                "openai/gpt-oss-20b",
                "tensorzero::model_name::llama-3.3-70b-versatile"
            ]
        );
        assert_eq!(
            report.mismatches[0].litellm_key,
            "openrouter/openai/gpt-oss-20b"
        );
        assert_eq!(report.mismatches[1].kind, RowKind::Route);
        assert_eq!(report.mismatches[1].litellm_key, "groq/openai/gpt-oss-120b");
        assert_eq!(report.compared, 5);
        assert_eq!(report.matched, 3);
        assert_eq!(report.not_found, ["acme/not-listed"]);
        assert_eq!(
            report.skipped,
            [
                "tensorzero::function_name::chat",
                "tensorzero::model_name::unknown-route"
            ]
        );
    }

    #[test]
    fn the_cross_check_never_changes_a_price() {
        let snapshot =
            CatalogSnapshot::new("r1", 1, vec![row("openai/gpt-oss-20b", 75_000, 300_000)]);
        let before = snapshot.clone();
        let prices = parse_litellm_prices(FIXTURE).unwrap();
        let report = cross_check(&snapshot, &prices, &routes(), 0);
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(snapshot, before);
        assert!(render_text(&report).contains("no price was changed"));
    }

    #[test]
    fn route_map_parses_the_canary_routes_file() {
        let legs =
            parse_route_legs(include_str!("../../../tools/provider-canary/routes.json")).unwrap();
        assert_eq!(legs["claude-haiku"][0].provider, "anthropic");
        assert_eq!(
            legs["claude-sonnet-5"][1].model,
            "anthropic/claude-sonnet-5"
        );
    }

    #[test]
    fn bad_input_fails_closed() {
        assert!(parse_litellm_prices("[]").is_err());
        assert!(parse_litellm_prices("{\"x\": {\"mode\": \"image\"}}").is_err());
        assert_eq!(diff_bps(0, 0), 0);
        assert_eq!(diff_bps(1, 0), u64::MAX);
        assert_eq!(diff_bps(105, 100), 500);
    }
}

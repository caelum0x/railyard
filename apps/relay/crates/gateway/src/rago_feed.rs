//! Rago (Lago-compatible) usage-event publisher for Relay cost events.
//!
//! With `RELAY_COST_FEED_FORMAT=rago`, the durable cost-event outbox delivers
//! each captured or metered inference to Rago's `POST /api/v1/events` as a
//! Lago-style usage event:
//!
//! ```json
//! {"event": {"transaction_id": "<event_id>", "external_subscription_id": "...",
//!            "code": "relay_llm_usage", "timestamp": 1700000000.123,
//!            "properties": {"model": "...", "input_tokens": 10, ...}}}
//! ```
//!
//! * `transaction_id` is the Relay `event_id`, so at-least-once redelivery is
//!   idempotent: Rago answers a replayed transaction with a 422
//!   `value_is_missing_or_already_exists`, which this publisher treats as
//!   delivered.
//! * `external_subscription_id` is resolved from the owning gateway API key:
//!   an explicit `RELAY_RAGO_SUBSCRIPTION_MAP` entry wins, otherwise
//!   `RELAY_RAGO_SUBSCRIPTION_PREFIX + <api key public id>`. Create the Rago
//!   subscription with that external id when provisioning the key.
//! * Multi-org routing: `RELAY_RAGO_ROUTES_JSON` maps a gateway API-key public
//!   id to its own Rago org bearer and subscription, so each consumer product
//!   (Rally/AutoGTM, OpenJev, Menivor, ...) is billed in its own Rago tenant:
//!
//!   ```json
//!   {"abcdefghijkl": {"api_key_env": "RAGO_KEY_AUTOGTM", "subscription_id": "autogtm-relay"},
//!    "mnopqrstuvwx": {"api_key": "rago_live_...", "subscription_id": "openjev-relay"}}
//!   ```
//!
//!   A routed key wins over the single-org settings. Unrouted keys fall back to
//!   `RELAY_COST_FEED_BEARER` plus the map/prefix above (the original
//!   single-org mode). With routes configured the default bearer is optional;
//!   an unrouted key with no default bearer fails delivery and is retried, then
//!   dead-lettered, rather than being billed to the wrong org.
//! * `code` is the billable metric (`RELAY_RAGO_METRIC_CODE`, default
//!   `relay_llm_usage`). Configure it in Rago as a `sum_agg` over
//!   `charged_micros` (retail spend) or `total_tokens`.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use relay_cost_feed::{CostEvent, CostEventPublisher, CostFeedError, Result};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_RAGO_METRIC_CODE: &str = "relay_llm_usage";
const RAGO_EVENTS_PATH: &str = "/api/v1/events";
const MICROS_PER_USD: i64 = 1_000_000;

/// Where one gateway API key's usage is billed: a Rago org (selected by its
/// bearer) and the subscription inside that org.
#[derive(Clone, PartialEq, Eq)]
pub struct RagoRoute {
    pub api_key: String,
    pub subscription_id: String,
}

impl std::fmt::Debug for RagoRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RagoRoute")
            .field("api_key", &"<redacted>")
            .field("subscription_id", &self.subscription_id)
            .finish()
    }
}

/// Resolved delivery target for one cost event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RagoTarget<'a> {
    pub api_key: &'a str,
    pub subscription_id: String,
}

/// Validated Rago delivery settings.
#[derive(Clone, PartialEq, Eq)]
pub struct RagoFeedConfig {
    pub events_url: Url,
    /// Single-org bearer used for keys without an explicit route.
    pub api_key: Option<String>,
    pub metric_code: String,
    pub subscription_prefix: String,
    pub subscription_map: BTreeMap<String, String>,
    /// Per-gateway-key routes to separate Rago orgs.
    pub routes: BTreeMap<String, RagoRoute>,
}

impl std::fmt::Debug for RagoFeedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RagoFeedConfig")
            .field("events_url", &self.events_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("metric_code", &self.metric_code)
            .field("subscription_prefix", &self.subscription_prefix)
            .field("subscription_map", &self.subscription_map)
            .field("routes", &self.routes)
            .finish()
    }
}

impl RagoFeedConfig {
    /// Build from the cost-feed URL/bearer plus `RELAY_RAGO_*` env vars.
    ///
    /// `base_url` may be the Rago origin (`https://rago.example.com`) or the full
    /// events endpoint; a bare origin gets `/api/v1/events` appended.
    pub fn from_env(base_url: &str, bearer: Option<String>) -> std::result::Result<Self, String> {
        let api_key = bearer
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let routes = match env_trimmed("RELAY_RAGO_ROUTES_JSON") {
            Some(raw) => parse_routes(&raw, env_trimmed)?,
            None => BTreeMap::new(),
        };
        let metric_code = env_trimmed("RELAY_RAGO_METRIC_CODE")
            .unwrap_or_else(|| DEFAULT_RAGO_METRIC_CODE.to_string());
        let subscription_prefix = env_trimmed("RELAY_RAGO_SUBSCRIPTION_PREFIX").unwrap_or_default();
        let subscription_map = match env_trimmed("RELAY_RAGO_SUBSCRIPTION_MAP") {
            Some(raw) => parse_subscription_map(&raw)?,
            None => BTreeMap::new(),
        };
        Self::new(
            base_url,
            api_key,
            metric_code,
            subscription_prefix,
            subscription_map,
            routes,
        )
    }

    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        metric_code: String,
        subscription_prefix: String,
        subscription_map: BTreeMap<String, String>,
        routes: BTreeMap<String, RagoRoute>,
    ) -> std::result::Result<Self, String> {
        if api_key.is_none() && routes.is_empty() {
            return Err(
                "RELAY_COST_FEED_FORMAT=rago requires RELAY_COST_FEED_BEARER (a Rago API key) \
                 or RELAY_RAGO_ROUTES_JSON"
                    .into(),
            );
        }
        let mut events_url =
            Url::parse(base_url).map_err(|error| format!("invalid Rago URL: {error}"))?;
        if !matches!(events_url.scheme(), "http" | "https") || events_url.host_str().is_none() {
            return Err("Rago URL must use http or https and include a host".into());
        }
        if !events_url.username().is_empty() || events_url.password().is_some() {
            return Err("Rago URL must not contain credentials".into());
        }
        if matches!(events_url.path(), "" | "/") {
            events_url.set_path(RAGO_EVENTS_PATH);
        }
        if api_key
            .as_deref()
            .is_some_and(|key| key.is_empty() || key.contains(['\r', '\n']))
        {
            return Err("Rago API key is empty or contains newlines".into());
        }
        for (key_id, route) in &routes {
            validate_route(key_id, route)?;
        }
        if metric_code.is_empty()
            || !metric_code
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err("RELAY_RAGO_METRIC_CODE must be a non-empty metric code".into());
        }
        Ok(Self {
            events_url,
            api_key,
            metric_code,
            subscription_prefix,
            subscription_map,
            routes,
        })
    }

    /// Rago external subscription id billed for a Relay API-key principal.
    pub fn subscription_for(&self, owner_principal: &str) -> String {
        if let Some(route) = self.routes.get(owner_principal) {
            return route.subscription_id.clone();
        }
        self.subscription_map
            .get(owner_principal)
            .cloned()
            .unwrap_or_else(|| format!("{}{owner_principal}", self.subscription_prefix))
    }

    /// The Rago org bearer and subscription for a Relay API-key principal, or
    /// `None` when the key has no route and there is no single-org bearer.
    pub fn target_for(&self, owner_principal: &str) -> Option<RagoTarget<'_>> {
        if let Some(route) = self.routes.get(owner_principal) {
            return Some(RagoTarget {
                api_key: &route.api_key,
                subscription_id: route.subscription_id.clone(),
            });
        }
        self.api_key.as_deref().map(|api_key| RagoTarget {
            api_key,
            subscription_id: self.subscription_for(owner_principal),
        })
    }

    /// The Lago-compatible event envelope for one Relay cost event.
    pub fn envelope(&self, event: &CostEvent) -> Value {
        let charged = event.charged_micros.get();
        let mut properties = json!({
            "request_id": event.request_id.to_string(),
            "api_key_id": event.owner_principal,
            "model": event.model,
            "route": event.route,
            "input_tokens": event.usage.input_tokens,
            "output_tokens": event.usage.output_tokens,
            "total_tokens": event.usage.total_tokens,
            "charged_micros": charged,
            "charged_usd": micros_to_usd_string(charged),
            "authorized_micros": event.authorized_micros.get(),
            "settlement": if event.is_metered() { "metered" } else { "mint" },
        });
        if let (Some(provider_cost), Some(object)) =
            (event.provider_cost_micros, properties.as_object_mut())
        {
            object.insert("provider_cost_micros".into(), json!(provider_cost.get()));
        }
        json!({
            "event": {
                "transaction_id": event.event_id.to_string(),
                "external_subscription_id": self.subscription_for(&event.owner_principal),
                "code": self.metric_code,
                "timestamp": event.settled_at_ms as f64 / 1_000.0,
                "properties": properties,
            }
        })
    }
}

/// Publishes Relay cost events into Rago's usage-event ingestion API.
pub struct RagoCostEventPublisher {
    client: reqwest::Client,
    config: RagoFeedConfig,
}

impl RagoCostEventPublisher {
    pub fn new(config: RagoFeedConfig) -> std::result::Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("relay-cost-feed/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| format!("build Rago client: {error}"))?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl CostEventPublisher for RagoCostEventPublisher {
    async fn publish(&self, event: &CostEvent) -> Result<()> {
        let target = self
            .config
            .target_for(&event.owner_principal)
            .ok_or_else(|| {
                CostFeedError::Publish(format!(
                    "no Rago route for API key {} and no default Rago API key",
                    event.owner_principal
                ))
            })?;
        let response = self
            .client
            .post(self.config.events_url.clone())
            .bearer_auth(target.api_key)
            .header("idempotency-key", event.event_id.to_string())
            .json(&self.config.envelope(event))
            .send()
            .await
            .map_err(|error| CostFeedError::Publish(format!("Rago request failed: {error}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() == 422 && is_duplicate_transaction(&body) {
            // Already ingested on an earlier (acknowledged-but-lost) attempt.
            return Ok(());
        }
        Err(CostFeedError::Publish(format!(
            "Rago returned HTTP {}: {}",
            status.as_u16(),
            summarize_rago_error(&body)
        )))
    }
}

/// Rago/Lago signal a replayed `transaction_id` as a 422 validation error.
fn is_duplicate_transaction(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.pointer("/error_details/transaction_id").cloned())
        .and_then(|codes| codes.as_array().cloned())
        .is_some_and(|codes| {
            codes
                .iter()
                .any(|code| code.as_str() == Some("value_is_missing_or_already_exists"))
        })
}

/// Keep dead-letter reasons actionable without storing unbounded bodies.
fn summarize_rago_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(200).collect();
    };
    let code = value.get("code").and_then(Value::as_str).unwrap_or("error");
    match value.get("error_details") {
        Some(details) => format!("{code} {details}").chars().take(300).collect(),
        None => code.to_string(),
    }
}

fn micros_to_usd_string(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let abs = micros.unsigned_abs();
    let per = MICROS_PER_USD.unsigned_abs();
    format!("{sign}{}.{:06}", abs / per, abs % per)
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_subscription_map(raw: &str) -> std::result::Result<BTreeMap<String, String>, String> {
    let map: BTreeMap<String, String> = serde_json::from_str(raw).map_err(|error| {
        format!(
            "RELAY_RAGO_SUBSCRIPTION_MAP must be a JSON object of api-key id to subscription id: {error}"
        )
    })?;
    if map
        .iter()
        .any(|(key, value)| key.trim().is_empty() || value.trim().is_empty())
    {
        return Err("RELAY_RAGO_SUBSCRIPTION_MAP entries must be non-empty".into());
    }
    Ok(map)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    api_key: Option<String>,
    api_key_env: Option<String>,
    subscription_id: String,
}

/// Parse `RELAY_RAGO_ROUTES_JSON`. `lookup` resolves `api_key_env` names so the
/// JSON itself can stay free of secrets.
fn parse_routes(
    raw: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> std::result::Result<BTreeMap<String, RagoRoute>, String> {
    let parsed: BTreeMap<String, RawRoute> = serde_json::from_str(raw).map_err(|error| {
        format!(
            "RELAY_RAGO_ROUTES_JSON must be a JSON object of api-key id to \
             {{\"api_key\" or \"api_key_env\", \"subscription_id\"}}: {error}"
        )
    })?;
    let mut routes = BTreeMap::new();
    for (key_id, route) in parsed {
        let api_key = match (route.api_key, route.api_key_env) {
            (Some(value), None) => value.trim().to_string(),
            (None, Some(name)) => lookup(name.trim()).ok_or_else(|| {
                format!("RELAY_RAGO_ROUTES_JSON route {key_id}: env var {name} is unset or empty")
            })?,
            _ => {
                return Err(format!(
                    "RELAY_RAGO_ROUTES_JSON route {key_id}: set exactly one of api_key or api_key_env"
                ));
            }
        };
        let route = RagoRoute {
            api_key,
            subscription_id: route.subscription_id.trim().to_string(),
        };
        validate_route(&key_id, &route)?;
        routes.insert(key_id.trim().to_string(), route);
    }
    Ok(routes)
}

fn validate_route(key_id: &str, route: &RagoRoute) -> std::result::Result<(), String> {
    if key_id.trim().is_empty() {
        return Err("RELAY_RAGO_ROUTES_JSON keys must be non-empty API-key ids".into());
    }
    if route.api_key.is_empty() || route.api_key.contains(['\r', '\n']) {
        return Err(format!(
            "RELAY_RAGO_ROUTES_JSON route {key_id}: Rago API key is empty or contains newlines"
        ));
    }
    if route.subscription_id.is_empty() {
        return Err(format!(
            "RELAY_RAGO_ROUTES_JSON route {key_id}: subscription_id must be non-empty"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use relay_contract::{Microdollars, Usage};
    use relay_cost_feed::InferenceRoute;
    use uuid::Uuid;

    use super::*;

    fn config(map: BTreeMap<String, String>) -> RagoFeedConfig {
        RagoFeedConfig::new(
            "https://rago.example.com",
            Some("rago-key".into()),
            DEFAULT_RAGO_METRIC_CODE.into(),
            "relay_".into(),
            map,
            BTreeMap::new(),
        )
        .expect("valid Rago config")
    }

    fn metered_event() -> CostEvent {
        metered_event_for("abcdefghijkl")
    }

    fn metered_event_for(owner: &str) -> CostEvent {
        CostEvent::metered(
            Uuid::now_v7(),
            owner,
            "tensorzero::model_name::gpt-4o-mini",
            InferenceRoute::OpenAiChatCompletions,
            Usage {
                input_tokens: 100,
                output_tokens: 20,
                total_tokens: 120,
                provider_cost_micros: None,
            },
            Microdollars::new(1_234_567),
            1_700_000_000_500,
        )
    }

    #[test]
    fn bare_origin_gets_events_path_and_prefix_mapping() {
        let config = config(BTreeMap::new());
        assert_eq!(config.events_url.path(), "/api/v1/events");
        assert_eq!(
            config.subscription_for("abcdefghijkl"),
            "relay_abcdefghijkl"
        );
    }

    #[test]
    fn explicit_subscription_map_wins() {
        let config = config(BTreeMap::from([(
            "abcdefghijkl".to_string(),
            "autogtm-org-42".to_string(),
        )]));
        assert_eq!(config.subscription_for("abcdefghijkl"), "autogtm-org-42");
    }

    #[test]
    fn envelope_matches_lago_event_contract() {
        let event = metered_event();
        assert!(event.validate().is_ok());
        let envelope = config(BTreeMap::new()).envelope(&event);
        let inner = &envelope["event"];
        assert_eq!(inner["transaction_id"], event.event_id.to_string());
        assert_eq!(inner["external_subscription_id"], "relay_abcdefghijkl");
        assert_eq!(inner["code"], DEFAULT_RAGO_METRIC_CODE);
        assert_eq!(inner["timestamp"], 1_700_000_000.5);
        assert_eq!(inner["properties"]["charged_micros"], 1_234_567);
        assert_eq!(inner["properties"]["charged_usd"], "1.234567");
        assert_eq!(inner["properties"]["settlement"], "metered");
        assert_eq!(inner["properties"]["total_tokens"], 120);
    }

    #[test]
    fn duplicate_transaction_is_detected() {
        let body = r#"{"status":422,"error":"Unprocessable Entity","code":"validation_errors","error_details":{"transaction_id":["value_is_missing_or_already_exists"]}}"#;
        assert!(is_duplicate_transaction(body));
        let other = r#"{"status":422,"code":"validation_errors","error_details":{"code":["value_is_invalid"]}}"#;
        assert!(!is_duplicate_transaction(other));
    }

    #[test]
    fn rejects_missing_key_and_bad_metric() {
        assert!(RagoFeedConfig::from_env("https://rago.example.com", None).is_err());
        assert!(
            RagoFeedConfig::new(
                "https://rago.example.com",
                Some("k".into()),
                "bad code".into(),
                String::new(),
                BTreeMap::new(),
                BTreeMap::new(),
            )
            .is_err()
        );
    }

    fn two_org_config() -> RagoFeedConfig {
        let routes = parse_routes(
            r#"{
                "autogtmkey01": {"api_key_env": "RAGO_KEY_AUTOGTM", "subscription_id": "autogtm-relay"},
                "openjevkey01": {"api_key": "rago-openjev", "subscription_id": "openjev-relay"}
            }"#,
            |name| (name == "RAGO_KEY_AUTOGTM").then(|| "rago-autogtm".to_string()),
        )
        .expect("valid routes");
        RagoFeedConfig::new(
            "https://rago.example.com",
            None,
            DEFAULT_RAGO_METRIC_CODE.into(),
            "relay_".into(),
            BTreeMap::new(),
            routes,
        )
        .expect("routes alone are enough")
    }

    #[test]
    fn routed_keys_resolve_to_separate_orgs() {
        let config = two_org_config();
        let autogtm = config.target_for("autogtmkey01").expect("routed");
        assert_eq!(autogtm.api_key, "rago-autogtm");
        assert_eq!(autogtm.subscription_id, "autogtm-relay");
        let openjev = config.target_for("openjevkey01").expect("routed");
        assert_eq!(openjev.api_key, "rago-openjev");
        assert_eq!(openjev.subscription_id, "openjev-relay");
        // No default bearer: an unrouted key is never billed to some other org.
        assert!(config.target_for("strangerkey1").is_none());
        let envelope = config.envelope(&metered_event_for("openjevkey01"));
        assert_eq!(
            envelope["event"]["external_subscription_id"],
            "openjev-relay"
        );
    }

    #[test]
    fn single_org_mode_still_works_alongside_routes() {
        let routes = BTreeMap::from([(
            "openjevkey01".to_string(),
            RagoRoute {
                api_key: "rago-openjev".into(),
                subscription_id: "openjev-relay".into(),
            },
        )]);
        let config = RagoFeedConfig::new(
            "https://rago.example.com",
            Some("rago-default".into()),
            DEFAULT_RAGO_METRIC_CODE.into(),
            "relay_".into(),
            BTreeMap::new(),
            routes,
        )
        .expect("valid");
        let fallback = config.target_for("abcdefghijkl").expect("default org");
        assert_eq!(fallback.api_key, "rago-default");
        assert_eq!(fallback.subscription_id, "relay_abcdefghijkl");
        assert_eq!(
            config.target_for("openjevkey01").expect("routed").api_key,
            "rago-openjev"
        );
    }

    #[test]
    fn invalid_routes_are_rejected() {
        let none = |_: &str| None;
        assert!(parse_routes("[]", none).is_err());
        assert!(parse_routes(r#"{"k": {"subscription_id": "s"}}"#, none).is_err());
        assert!(
            parse_routes(
                r#"{"k": {"api_key": "a", "api_key_env": "B", "subscription_id": "s"}}"#,
                none
            )
            .is_err()
        );
        assert!(
            parse_routes(
                r#"{"k": {"api_key_env": "MISSING", "subscription_id": "s"}}"#,
                none
            )
            .is_err()
        );
        assert!(parse_routes(r#"{"k": {"api_key": "a", "subscription_id": " "}}"#, none).is_err());
        assert!(parse_routes(r#"{" ": {"api_key": "a", "subscription_id": "s"}}"#, none).is_err());
        assert!(
            parse_routes(
                r#"{"k": {"api_key": "a", "subscription_id": "s", "x": 1}}"#,
                none
            )
            .is_err()
        );
    }

    #[test]
    fn debug_output_redacts_bearers() {
        let rendered = format!("{:?}", two_org_config());
        assert!(!rendered.contains("rago-autogtm"));
        assert!(!rendered.contains("rago-openjev"));
        assert!(rendered.contains("openjev-relay"));
    }

    /// End to end over HTTP: two gateway keys land in two Rago orgs.
    #[tokio::test]
    async fn two_keys_publish_into_two_orgs() {
        use std::sync::{Arc, Mutex};

        use axum::{Router, extract::State, http::HeaderMap, routing::post};

        type Seen = Arc<Mutex<Vec<(String, Value)>>>;
        async fn capture(
            State(seen): State<Seen>,
            headers: HeaderMap,
            axum::Json(body): axum::Json<Value>,
        ) -> axum::http::StatusCode {
            let bearer = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            seen.lock().expect("lock").push((bearer, body));
            axum::http::StatusCode::OK
        }

        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/api/v1/events", post(capture))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        #[expect(
            clippy::disallowed_methods,
            reason = "test-only mock Rago server, aborted at the end of the test"
        )]
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let mut config = two_org_config();
        config.events_url = Url::parse(&format!("http://{addr}/api/v1/events")).expect("url");
        let publisher = RagoCostEventPublisher::new(config).expect("publisher");
        publisher
            .publish(&metered_event_for("autogtmkey01"))
            .await
            .expect("autogtm delivered");
        publisher
            .publish(&metered_event_for("openjevkey01"))
            .await
            .expect("openjev delivered");
        let unrouted = publisher.publish(&metered_event_for("strangerkey1")).await;
        assert!(
            unrouted.is_err(),
            "unrouted key must not be billed anywhere"
        );
        server.abort();

        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "Bearer rago-autogtm");
        assert_eq!(
            seen[0].1["event"]["external_subscription_id"],
            "autogtm-relay"
        );
        assert_eq!(seen[1].0, "Bearer rago-openjev");
        assert_eq!(
            seen[1].1["event"]["external_subscription_id"],
            "openjev-relay"
        );
        assert_eq!(seen[1].1["event"]["code"], DEFAULT_RAGO_METRIC_CODE);
    }
}

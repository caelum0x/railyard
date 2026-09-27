//! Provider availability: a provider whose key is absent is DISABLED, not fatal.
//!
//! Relay boots with any subset of provider credentials (the runtime config only
//! references `dynamic::relay_*_api_key` slots, see `provider_keys.rs`). This
//! module decides, per inference request and before any provider I/O, whether
//! the requested target can run at all:
//!
//! * `tensorzero::model_name::<provider>::<model>` (shorthand) needs that
//!   provider's slot. When the slot has no key but the operator catalog defines
//!   a model named `<model>` that IS servable, the request is resolved to that
//!   catalog entry (e.g. Menivor sends `openai::llama-3.3-70b-versatile` for
//!   Groq models; with no OpenAI key it runs on the Groq-backed catalog model).
//!   When the slot HAS a key and is the first route of a same-id catalog model
//!   with another enabled route, the request also runs on that catalog model,
//!   so a failure at the requested provider fails over to the paid floor.
//! * `tensorzero::model_name::<name>` (catalog model) needs ANY provider in its
//!   routing to have a key (routing fails over to the next one).
//! * `tensorzero::function_name::<name>` needs ANY chat-completion variant whose
//!   model is servable. Functions with other variant kinds are not judged here.
//!
//! A target that cannot run gets HTTP 503 with code `provider_not_configured`
//! naming the disabled providers, instead of an opaque provider/auth failure.
//! Provider types without a Relay slot are never judged (the core handles them).

use std::collections::BTreeSet;

use serde_json::Value;
use tensorzero_core::config::Config;
use tensorzero_core::variant::VariantConfig;

/// Error code returned when a request needs only disabled providers.
pub const PROVIDER_NOT_CONFIGURED: &str = "provider_not_configured";

const MODEL_PREFIX: &str = "tensorzero::model_name::";
const FUNCTION_PREFIX: &str = "tensorzero::function_name::";

/// What an inference request asks the gateway to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceTarget {
    /// A model name as the core resolves it: catalog entry or `provider::model`.
    Model(String),
    Function(String),
}

/// Where the target lives in the request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyShape {
    /// OpenAI-compatible `/openai/v1/chat/completions`: `model` carries the
    /// `tensorzero::model_name::` / `tensorzero::function_name::` prefix.
    OpenAiChat,
    /// Native `/inference`: `model_name` or `function_name`.
    Native,
}

/// Read the target from an inference request body.
pub fn target_from_body(shape: BodyShape, body: &Value) -> Option<InferenceTarget> {
    match shape {
        BodyShape::OpenAiChat => {
            let model = body.get("model")?.as_str()?.trim();
            if let Some(name) = model.strip_prefix(MODEL_PREFIX) {
                Some(InferenceTarget::Model(name.to_string()))
            } else {
                model
                    .strip_prefix(FUNCTION_PREFIX)
                    .map(|name| InferenceTarget::Function(name.to_string()))
            }
        }
        BodyShape::Native => {
            if let Some(name) = body.get("model_name").and_then(Value::as_str) {
                Some(InferenceTarget::Model(name.trim().to_string()))
            } else {
                body.get("function_name")
                    .and_then(Value::as_str)
                    .map(|name| InferenceTarget::Function(name.trim().to_string()))
            }
        }
    }
}

/// Read-only view of the routing the loaded gateway config defines.
pub trait RouteLookup {
    /// Provider types of a catalog model's routing, in order; `None` when the
    /// config has no model with this exact name.
    fn model_provider_types(&self, name: &str) -> Option<Vec<String>>;
    /// Models of a function's chat-completion variants. `Some(None)` when the
    /// function has a variant kind this check does not model; `None` when the
    /// config has no such function.
    fn function_models(&self, name: &str) -> Option<Option<Vec<String>>>;
}

impl RouteLookup for Config {
    fn model_provider_types(&self, name: &str) -> Option<Vec<String>> {
        let model = self.models.table.get(name)?;
        Some(
            model
                .routing
                .iter()
                .filter_map(|provider| model.providers.get(provider))
                .map(|provider| provider.provider_type().to_string())
                .collect(),
        )
    }

    fn function_models(&self, name: &str) -> Option<Option<Vec<String>>> {
        let function = self.functions.get(name)?;
        let mut models = Vec::new();
        for variant in function.variants().values() {
            match &variant.inner {
                VariantConfig::ChatCompletion(chat) => models.push(chat.model().to_string()),
                _ => return Some(None),
            }
        }
        Some(Some(models))
    }
}

/// The availability verdict for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Run the request unchanged.
    Servable,
    /// Run the request with the OpenAI/native model rewritten to this catalog
    /// model name (a shorthand whose provider is disabled).
    ResolveTo(String),
    /// Run the request on this catalog model: the shorthand's provider is
    /// enabled and is the catalog model's FIRST route, so the first attempt is
    /// unchanged, but a failure there (rate limit, exhausted quota, outage)
    /// now fails over to the catalog model's later routes (the paid floor).
    FailoverTo(String),
    /// Refuse: every route needs one of these disabled provider types.
    NotConfigured(Vec<String>),
}

/// Decide whether `target` can run.
///
/// `has_slot(provider_type)` says whether Relay manages a key slot for that
/// provider type; `enabled(provider_type)` whether that slot currently has a
/// key (stored, environment, or supplied by the caller).
pub fn evaluate(
    target: &InferenceTarget,
    routes: &dyn RouteLookup,
    has_slot: &dyn Fn(&str) -> bool,
    enabled: &dyn Fn(&str) -> bool,
) -> Availability {
    match target {
        InferenceTarget::Model(name) => {
            let mut missing = BTreeSet::new();
            if model_servable(name, routes, has_slot, enabled, &mut missing) {
                return failover_twin(name, routes, has_slot, enabled)
                    .map_or(Availability::Servable, Availability::FailoverTo);
            }
            // A shorthand whose provider is disabled resolves to the catalog
            // model of the same id when that one can run.
            if let Some((_, model)) = split_shorthand(name, has_slot)
                && routes.model_provider_types(model).is_some()
                && model_servable(model, routes, has_slot, enabled, &mut BTreeSet::new())
            {
                return Availability::ResolveTo(model.to_string());
            }
            Availability::NotConfigured(missing.into_iter().collect())
        }
        InferenceTarget::Function(name) => {
            let Some(Some(models)) = routes.function_models(name) else {
                return Availability::Servable;
            };
            if models.is_empty() {
                return Availability::Servable;
            }
            let mut missing = BTreeSet::new();
            if models
                .iter()
                .any(|model| model_servable(model, routes, has_slot, enabled, &mut missing))
            {
                Availability::Servable
            } else {
                Availability::NotConfigured(missing.into_iter().collect())
            }
        }
    }
}

/// The catalog model a servable shorthand should run on so it can fail over.
///
/// Consumers send `groq::<model>` shorthands, which the core sends to that one
/// provider with no fallback. When the catalog defines a model with the same id
/// whose routing STARTS with that provider and has at least one other enabled
/// route, running the catalog model keeps the first attempt identical and adds
/// the later routes as a floor. Anything else is left exactly as requested.
fn failover_twin(
    name: &str,
    routes: &dyn RouteLookup,
    has_slot: &dyn Fn(&str) -> bool,
    enabled: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let (provider, model) = split_shorthand(name, has_slot)?;
    let providers = routes.model_provider_types(model)?;
    let (first, rest) = providers.split_first()?;
    let has_floor = rest
        .iter()
        .any(|later| later != provider && (!has_slot(later) || enabled(later)));
    (first == provider && has_floor).then(|| model.to_string())
}

/// `provider::model` when `name` is a shorthand for a Relay-managed provider.
fn split_shorthand<'a>(
    name: &'a str,
    has_slot: &dyn Fn(&str) -> bool,
) -> Option<(&'a str, &'a str)> {
    let (provider, model) = name.split_once("::")?;
    (has_slot(provider) && !model.is_empty()).then_some((provider, model))
}

fn model_servable(
    name: &str,
    routes: &dyn RouteLookup,
    has_slot: &dyn Fn(&str) -> bool,
    enabled: &dyn Fn(&str) -> bool,
    missing: &mut BTreeSet<String>,
) -> bool {
    // Catalog entries win over shorthand parsing, exactly like the core.
    if let Some(providers) = routes.model_provider_types(name) {
        if providers.is_empty() {
            return true;
        }
        let mut servable = false;
        for provider in providers {
            if !has_slot(&provider) || enabled(&provider) {
                servable = true;
            } else {
                missing.insert(provider);
            }
        }
        return servable;
    }
    match split_shorthand(name, has_slot) {
        Some((provider, _)) if !enabled(provider) => {
            missing.insert(provider.to_string());
            false
        }
        // Enabled shorthand, an unmanaged provider type, or an unknown name
        // (the core reports unknown models itself).
        _ => true,
    }
}

/// Rewrite the request's target model to a catalog model name.
pub fn rewrite_model(shape: BodyShape, body: &mut serde_json::Map<String, Value>, model: &str) {
    match shape {
        BodyShape::OpenAiChat => {
            body.insert(
                "model".into(),
                Value::String(format!("{MODEL_PREFIX}{model}")),
            );
        }
        BodyShape::Native => {
            body.insert("model_name".into(), Value::String(model.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    struct Fake {
        models: HashMap<&'static str, Vec<&'static str>>,
        functions: HashMap<&'static str, Option<Vec<&'static str>>>,
    }

    impl RouteLookup for Fake {
        fn model_provider_types(&self, name: &str) -> Option<Vec<String>> {
            self.models
                .get(name)
                .map(|providers| providers.iter().map(ToString::to_string).collect())
        }
        fn function_models(&self, name: &str) -> Option<Option<Vec<String>>> {
            self.functions.get(name).map(|models| {
                models
                    .as_ref()
                    .map(|models| models.iter().map(ToString::to_string).collect())
            })
        }
    }

    fn relay() -> Fake {
        Fake {
            models: HashMap::from([
                ("llama-3.3-70b-versatile", vec!["groq", "openrouter"]),
                ("gpt-4o-mini", vec!["openai", "openrouter"]),
                ("claude-sonnet", vec!["anthropic", "openrouter"]),
                ("local", vec!["vllm"]),
            ]),
            functions: HashMap::from([
                (
                    "chat",
                    Some(vec![
                        "llama-3.3-70b-versatile",
                        "gpt-4o-mini",
                        "claude-sonnet",
                    ]),
                ),
                ("paid", Some(vec!["gpt-4o-mini"])),
                ("fancy", None),
            ]),
        }
    }

    const SLOTS: &[&str] = &["openai", "anthropic", "openrouter", "groq"];

    fn has_slot(provider: &str) -> bool {
        SLOTS.contains(&provider)
    }

    fn check(target: InferenceTarget, keys: &[&str]) -> Availability {
        let enabled = |provider: &str| keys.contains(&provider);
        evaluate(&target, &relay(), &has_slot, &enabled)
    }

    fn model(name: &str) -> InferenceTarget {
        InferenceTarget::Model(name.to_string())
    }

    #[test]
    fn groq_only_serves_groq_shorthand_and_refuses_others_with_their_provider() {
        let groq = &["groq"];
        assert_eq!(
            check(model("groq::llama-3.3-70b-versatile"), groq),
            Availability::Servable
        );
        assert_eq!(
            check(
                model("openrouter::meta-llama/llama-3.3-70b-instruct:free"),
                groq
            ),
            Availability::NotConfigured(vec!["openrouter".into()])
        );
        assert_eq!(
            check(model("openai::gpt-4o-mini"), groq),
            Availability::NotConfigured(vec!["openai".into()])
        );
    }

    #[test]
    fn enabled_shorthand_gains_the_catalog_floor_when_its_provider_routes_first() {
        // Groq first, OpenRouter floor enabled: run the catalog model so a
        // Groq failure fails over instead of reaching the caller.
        assert_eq!(
            check(
                model("groq::llama-3.3-70b-versatile"),
                &["groq", "openrouter"]
            ),
            Availability::FailoverTo("llama-3.3-70b-versatile".into())
        );
        assert_eq!(
            check(model("openai::gpt-4o-mini"), &["openai", "openrouter"]),
            Availability::FailoverTo("gpt-4o-mini".into())
        );
        // No enabled later route: nothing to fail over to, request unchanged.
        assert_eq!(
            check(model("groq::llama-3.3-70b-versatile"), &["groq"]),
            Availability::Servable
        );
        // The shorthand's provider is not the catalog's first route: the
        // caller's explicit provider choice stands.
        assert_eq!(
            check(
                model("openrouter::llama-3.3-70b-versatile"),
                &["groq", "openrouter"]
            ),
            Availability::Servable
        );
        // No catalog twin, unmanaged provider or catalog name: unchanged.
        assert_eq!(
            check(
                model("groq::openai/gpt-oss-safeguard-20b"),
                &["groq", "openrouter"]
            ),
            Availability::Servable
        );
        assert_eq!(
            check(model("mistral::large"), &["groq", "openrouter"]),
            Availability::Servable
        );
        assert_eq!(
            check(model("gpt-4o-mini"), &["openai", "openrouter"]),
            Availability::Servable
        );
    }

    #[test]
    fn catalog_models_need_any_routed_provider() {
        assert_eq!(
            check(model("gpt-4o-mini"), &["openrouter"]),
            Availability::Servable
        );
        assert_eq!(
            check(model("gpt-4o-mini"), &["groq"]),
            Availability::NotConfigured(vec!["openai".into(), "openrouter".into()])
        );
        // Unmanaged provider types are never judged here.
        assert_eq!(check(model("local"), &[]), Availability::Servable);
        // Unknown names are left to the core's own error.
        assert_eq!(check(model("mystery"), &[]), Availability::Servable);
        assert_eq!(check(model("mistral::large"), &[]), Availability::Servable);
    }

    #[test]
    fn disabled_shorthand_resolves_to_a_servable_catalog_model_of_the_same_id() {
        assert_eq!(
            check(model("openai::llama-3.3-70b-versatile"), &["groq"]),
            Availability::ResolveTo("llama-3.3-70b-versatile".into())
        );
        // With the OpenAI key present the caller's explicit choice stands.
        assert_eq!(
            check(model("openai::llama-3.3-70b-versatile"), &["openai"]),
            Availability::Servable
        );
        // No servable catalog twin: refused, naming the requested provider.
        assert_eq!(
            check(model("openai::llama-3.3-70b-versatile"), &["anthropic"]),
            Availability::NotConfigured(vec!["openai".into()])
        );
    }

    #[test]
    fn functions_need_any_servable_variant_model() {
        let chat = InferenceTarget::Function("chat".into());
        assert_eq!(check(chat.clone(), &["groq"]), Availability::Servable);
        assert_eq!(check(chat.clone(), &["anthropic"]), Availability::Servable);
        assert_eq!(
            check(chat, &[]),
            Availability::NotConfigured(vec![
                "anthropic".into(),
                "groq".into(),
                "openai".into(),
                "openrouter".into()
            ])
        );
        assert_eq!(
            check(InferenceTarget::Function("paid".into()), &["groq"]),
            Availability::NotConfigured(vec!["openai".into(), "openrouter".into()])
        );
        assert_eq!(
            check(InferenceTarget::Function("fancy".into()), &[]),
            Availability::Servable
        );
        assert_eq!(
            check(InferenceTarget::Function("unknown".into()), &[]),
            Availability::Servable
        );
    }

    #[test]
    fn targets_are_read_from_both_body_shapes() {
        assert_eq!(
            target_from_body(
                BodyShape::OpenAiChat,
                &json!({"model": "tensorzero::model_name::groq::llama-3.1-8b-instant"})
            ),
            Some(model("groq::llama-3.1-8b-instant"))
        );
        assert_eq!(
            target_from_body(
                BodyShape::OpenAiChat,
                &json!({"model": "tensorzero::function_name::chat"})
            ),
            Some(InferenceTarget::Function("chat".into()))
        );
        assert_eq!(
            target_from_body(BodyShape::OpenAiChat, &json!({"model": "gpt-4o"})),
            None
        );
        assert_eq!(
            target_from_body(BodyShape::Native, &json!({"model_name": "gpt-4o-mini"})),
            Some(model("gpt-4o-mini"))
        );
        assert_eq!(
            target_from_body(BodyShape::Native, &json!({"function_name": "chat"})),
            Some(InferenceTarget::Function("chat".into()))
        );
    }

    #[test]
    fn rewrite_keeps_each_shape_valid() {
        let mut openai = serde_json::Map::new();
        rewrite_model(
            BodyShape::OpenAiChat,
            &mut openai,
            "llama-3.3-70b-versatile",
        );
        assert_eq!(
            openai["model"],
            json!("tensorzero::model_name::llama-3.3-70b-versatile")
        );
        let mut native = serde_json::Map::new();
        rewrite_model(BodyShape::Native, &mut native, "llama-3.3-70b-versatile");
        assert_eq!(native["model_name"], json!("llama-3.3-70b-versatile"));
    }
}

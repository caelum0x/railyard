//! Shared domain identifiers and chat primitives.
//!
//! Frozen vocabulary: account / key / job identifiers, the OpenRouter-namespaced
//! model id, and the chat message shape. Pure data — no I/O.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The `openrouter/` catalog namespace prefix.
pub const OPENROUTER_PREFIX: &str = "openrouter/";

/// A tenant / wallet account identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl AccountId {
    pub fn new(id: impl Into<String>) -> Self {
        AccountId(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A stored API-key identifier (distinct from the secret key material).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyId(pub String);

impl KeyId {
    pub fn new(id: impl Into<String>) -> Self {
        KeyId(id.into())
    }
    /// Mint a fresh random key id.
    pub fn generate() -> Self {
        KeyId(Uuid::now_v7().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A billable job identifier (one chat/completion request = one job).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub Uuid);

impl JobId {
    /// Mint a fresh v4 job id.
    pub fn new() -> Self {
        JobId(Uuid::now_v7())
    }
    pub fn from_uuid(id: Uuid) -> Self {
        JobId(id)
    }
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Parse a caller-propagated request id. Relay only accepts UUIDs at this
    /// boundary, keeping header values bounded and canonical.
    pub fn parse(id: &str) -> Option<Self> {
        Uuid::parse_str(id).ok().map(JobId)
    }
}

impl Default for JobId {
    fn default() -> Self {
        JobId::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// A catalog model id, always carried in namespaced `openrouter/{raw}` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    /// Wrap an already-catalog-shaped id (namespaced or not) verbatim.
    pub fn new(catalog_id: impl Into<String>) -> Self {
        ModelId(catalog_id.into())
    }

    /// Namespace a raw upstream id into the `openrouter/{raw}` catalog form.
    /// Idempotent: an already-namespaced id is returned unchanged.
    pub fn namespace(raw: &str) -> Self {
        if raw.starts_with(OPENROUTER_PREFIX) {
            ModelId(raw.to_string())
        } else {
            ModelId(format!("{OPENROUTER_PREFIX}{raw}"))
        }
    }

    /// Convert TensorZero's OpenAI-compatible concrete-model selector into the
    /// catalog key used by Relay billing.
    ///
    /// `tensorzero::model_name::<provider>::<model>` becomes
    /// `openrouter/<provider>/<model>`. Only the first separator after the
    /// model prefix is structural; the model tail may itself contain `::`.
    /// Function selectors retain their function identity but drop the selected
    /// `variant_name` suffix. Their concrete provider model cannot be inferred,
    /// but this stable key lets an operator configure a conservative price for
    /// the function independent of which variant TensorZero selected.
    pub fn from_tensorzero_selector(raw: &str) -> Self {
        const MODEL_PREFIX: &str = "tensorzero::model_name::";
        const FUNCTION_PREFIX: &str = "tensorzero::function_name::";

        let candidate = match raw.strip_prefix(MODEL_PREFIX) {
            Some(model) => match model.split_once("::") {
                Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                    return Self::namespace(&format!("{provider}/{model}"));
                }
                _ if !model.is_empty() => {
                    // A configured TensorZero model name is a retail route, not
                    // a provider/model identifier. Preserve that identity so
                    // operators can price it explicitly and independently of
                    // whichever providers its configuration contains.
                    return Self::namespace(&format!("{MODEL_PREFIX}{model}"));
                }
                _ => raw,
            },
            None if raw.starts_with(FUNCTION_PREFIX) => {
                let function = raw
                    .strip_prefix(FUNCTION_PREFIX)
                    .expect("prefix checked above");
                let function = function
                    .split_once("::variant_name::")
                    .map_or(function, |(function, _variant)| function);
                if function.is_empty() {
                    return Self::namespace(raw);
                }
                return Self::namespace(&format!("{FUNCTION_PREFIX}{function}"));
            }
            None => raw,
        };
        match candidate.split_once("::") {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                Self::namespace(&format!("{provider}/{model}"))
            }
            _ => Self::namespace(candidate),
        }
    }

    /// Strip the `openrouter/` prefix, yielding the raw id the upstream expects.
    pub fn raw_upstream_id(&self) -> &str {
        self.0.strip_prefix(OPENROUTER_PREFIX).unwrap_or(&self.0)
    }

    /// The full catalog id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this id represents a TensorZero function rather than one
    /// concrete provider model. Function selectors must have an explicit
    /// operator price because their chosen variant can change at runtime.
    pub fn is_tensorzero_function(&self) -> bool {
        self.raw_upstream_id()
            .starts_with("tensorzero::function_name::")
    }

    /// Whether this selector is an operator-configured TensorZero route whose
    /// provider choice cannot be inferred from its public name.
    pub fn requires_explicit_price(&self) -> bool {
        let raw = self.raw_upstream_id();
        raw.starts_with("tensorzero::function_name::")
            || raw.starts_with("tensorzero::model_name::")
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The role of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A single chat message in an OpenAI-compatible conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: content.into(),
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage::new(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage::new(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage::new(Role::Assistant, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_namespace_and_strip_roundtrip() {
        let m = ModelId::namespace("anthropic/claude-3.5-sonnet");
        assert_eq!(m.as_str(), "openrouter/anthropic/claude-3.5-sonnet");
        assert_eq!(m.raw_upstream_id(), "anthropic/claude-3.5-sonnet");
    }

    #[test]
    fn model_namespace_is_idempotent() {
        let already = ModelId::namespace("openrouter/openai/gpt-4o");
        assert_eq!(already.as_str(), "openrouter/openai/gpt-4o");
        assert_eq!(already.raw_upstream_id(), "openai/gpt-4o");
    }

    #[test]
    fn model_new_without_prefix_strips_to_self() {
        let m = ModelId::new("plain-model");
        assert_eq!(m.raw_upstream_id(), "plain-model");
    }

    #[test]
    fn tensorzero_selectors_map_to_stable_catalog_identities() {
        assert_eq!(
            ModelId::from_tensorzero_selector("tensorzero::model_name::openai::gpt-4o"),
            ModelId::namespace("openai/gpt-4o")
        );
        assert_eq!(
            ModelId::from_tensorzero_selector(
                "tensorzero::model_name::openai::responses::gpt-5-mini"
            ),
            ModelId::namespace("openai/responses::gpt-5-mini")
        );
        assert_eq!(
            ModelId::from_tensorzero_selector("tensorzero::model_name::openrouter::openai/gpt-4o"),
            ModelId::namespace("openai/gpt-4o")
        );
        let configured_model =
            ModelId::from_tensorzero_selector("tensorzero::model_name::production-chat");
        assert_eq!(
            configured_model,
            ModelId::namespace("tensorzero::model_name::production-chat")
        );
        assert!(configured_model.requires_explicit_price());
        let function = "tensorzero::function_name::write::variant_name::quality";
        assert_eq!(
            ModelId::from_tensorzero_selector(function),
            ModelId::namespace("tensorzero::function_name::write")
        );
        assert_eq!(
            ModelId::from_tensorzero_selector("tensorzero::function_name::write"),
            ModelId::namespace("tensorzero::function_name::write")
        );
        assert!(
            ModelId::from_tensorzero_selector("tensorzero::function_name::write")
                .is_tensorzero_function()
        );
        assert!(
            ModelId::from_tensorzero_selector("tensorzero::function_name::write")
                .requires_explicit_price()
        );
        assert!(!ModelId::namespace("openai/gpt-4o").is_tensorzero_function());
        assert!(!ModelId::namespace("openai/gpt-4o").requires_explicit_price());
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn chat_message_roundtrips_through_json() {
        let m = ChatMessage::user("hello");
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"role":"user","content":"hello"}"#);
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn job_ids_are_unique() {
        assert_ne!(JobId::new(), JobId::new());
    }
}

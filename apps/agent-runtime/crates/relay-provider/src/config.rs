//! Relay connection config + model-name prefixing.
//!
//! Relay is a TensorZero gateway exposing an OpenAI-compatible endpoint at
//! `POST {base}/openai/v1/chat/completions`. Two hard requirements the vendored
//! rig openai client does not know about:
//!   1. auth is `Authorization: Bearer <key>`; and
//!   2. the `model` field MUST be prefixed `tensorzero::model_name::<model>`
//!      (or `tensorzero::function_name::<fn>`), else Relay returns 400.

/// Path segment rig's openai Chat Completions client appends is
/// `/chat/completions`, so the configured base URL must end at `/openai/v1`.
pub const OPENAI_COMPAT_SUFFIX: &str = "/openai/v1";

/// Required prefix for a bare model id (SPEC of Relay's OpenAI-compat layer).
pub const MODEL_NAME_PREFIX: &str = "tensorzero::model_name::";

/// Required prefix for calling a named TensorZero function instead of a model.
pub const FUNCTION_NAME_PREFIX: &str = "tensorzero::function_name::";

/// Everything needed to point rig's openai Chat Completions client at Relay.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Base URL up to and including `/openai/v1` (rig appends `/chat/completions`).
    pub base_url: String,
    /// Bearer token for Relay's auth middleware.
    pub api_key: String,
    /// The rig `model` string, already Relay-prefixed. Build via
    /// [`RelayConfig::model_name`] / [`RelayConfig::function_name`].
    pub model: String,
}

impl RelayConfig {
    /// Config for a concrete model id, e.g. `openai::gpt-4o-mini` →
    /// `tensorzero::model_name::openai::gpt-4o-mini`.
    pub fn model_name(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl AsRef<str>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: format!("{MODEL_NAME_PREFIX}{}", model_id.as_ref()),
        }
    }

    /// Config for a named TensorZero function.
    pub fn function_name(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        function: impl AsRef<str>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: format!("{FUNCTION_NAME_PREFIX}{}", function.as_ref()),
        }
    }
}

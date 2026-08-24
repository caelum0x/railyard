//! `relay-provider`: a rig [`rig_core::completion::CompletionModel`] backed by
//! Relay's OpenAI-compatible (TensorZero) gateway.
//!
//! The genuine reuse of the vendored `external/rig` copy lives here: [`RelayModel`]
//! wraps rig's openai Chat Completions client pointed at Relay. Tests wrap a
//! `MockCompletionModel` instead so nothing touches the network.

pub mod config;
pub mod error;
pub mod model;

pub use config::{RelayConfig, FUNCTION_NAME_PREFIX, MODEL_NAME_PREFIX, OPENAI_COMPAT_SUFFIX};
pub use error::RelayError;
pub use model::{OpenAiCompletions, RelayModel};

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::{AssistantContent, CompletionModel};
    use rig_core::test_utils::MockCompletionModel;

    #[test]
    fn model_name_is_relay_prefixed() {
        let cfg = RelayConfig::model_name("http://localhost/openai/v1", "k", "openai::gpt-4o-mini");
        assert_eq!(cfg.model, "tensorzero::model_name::openai::gpt-4o-mini");
    }

    #[test]
    fn connect_builds_a_live_model() {
        // No network: constructing the client does not call Relay.
        let cfg = RelayConfig::model_name("http://localhost:3000/openai/v1", "test-key", "dummy");
        let model = RelayModel::connect(&cfg).expect("client builds");
        // Delegates rig's default capabilities without panicking.
        let _ = model.capabilities();
    }

    #[tokio::test]
    async fn wraps_a_mock_for_offline_completion() {
        let mock = MockCompletionModel::text("hello from relay");
        let model = RelayModel::from_inner(mock);
        // The wrapper is a CompletionModel: build a request and complete it,
        // entirely offline.
        let request = model.completion_request("hi").build();
        let response = model.completion(request).await.expect("mock replies");
        let text = response.choice.iter().find_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("hello from relay"));
    }
}

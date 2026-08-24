//! [`ScriptedUpstream`] — a fake provider forwarder for tests.
//!
//! Returns canned responses FIFO: success-with-usage, non-2xx failure, and a
//! simulated timeout (surfaced as [`AbacusError::Transport`]). Mirrors
//! `vend-testkit::FakeRelay`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use abacus_core::error::AbacusError;
use abacus_core::transport::{UpstreamRequest, UpstreamResponse, UpstreamTransport, Usage};

/// A scripted step: either a response to return or a simulated timeout.
enum Step {
    Respond(UpstreamResponse),
    Timeout,
}

/// A scriptable in-memory upstream. Pushed steps are consumed FIFO; when the
/// script is exhausted it returns [`ScriptedUpstream::default_response`].
pub struct ScriptedUpstream {
    scripted: Mutex<Vec<Step>>,
    default_response: UpstreamResponse,
    /// How many times `send` was invoked — lets tests prove a KILL forwarded
    /// ZERO bytes (no upstream call) before any hold was captured.
    calls: AtomicUsize,
}

impl Default for ScriptedUpstream {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedUpstream {
    /// An upstream that returns a canned 200 with 1/1 token usage by default.
    pub fn new() -> Self {
        ScriptedUpstream {
            scripted: Mutex::new(Vec::new()),
            default_response: UpstreamResponse {
                status: 200,
                body: serde_json::json!({"choices": []}),
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                }),
            },
            calls: AtomicUsize::new(0),
        }
    }

    /// How many times the upstream was actually called. `0` proves a KILL
    /// short-circuited before any provider byte was forwarded.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Script the next response (FIFO).
    pub fn push_response(self, resp: UpstreamResponse) -> Self {
        self.scripted.lock().unwrap().insert(0, Step::Respond(resp));
        self
    }

    /// Script the next call to time out (surfaced as a transport error).
    pub fn push_timeout(self) -> Self {
        self.scripted.lock().unwrap().insert(0, Step::Timeout);
        self
    }

    /// A canned 2xx response carrying the given usage.
    pub fn success_with_usage(usage: Usage) -> UpstreamResponse {
        UpstreamResponse {
            status: 200,
            body: serde_json::json!({"choices": [], "usage": {
                "prompt_tokens": usage.input_tokens,
                "completion_tokens": usage.output_tokens,
            }}),
            usage: Some(usage),
        }
    }

    /// A canned non-2xx (no usage) failure response.
    pub fn failure(status: u16) -> UpstreamResponse {
        UpstreamResponse {
            status,
            body: serde_json::json!({"error": "upstream"}),
            usage: None,
        }
    }
}

#[async_trait]
impl UpstreamTransport for ScriptedUpstream {
    async fn send(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, AbacusError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = self.scripted.lock().unwrap().pop();
        match step {
            Some(Step::Respond(resp)) => Ok(resp),
            Some(Step::Timeout) => Err(AbacusError::Transport("upstream timed out".into())),
            None => Ok(self.default_response.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_success_then_failure_then_timeout() {
        let up = ScriptedUpstream::new()
            .push_response(ScriptedUpstream::success_with_usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
            }))
            .push_response(ScriptedUpstream::failure(500))
            .push_timeout();

        let req = || UpstreamRequest {
            path: "/v1/chat/completions".into(),
            bearer: "k".into(),
            body: serde_json::json!({}),
        };

        let ok = up.send(req()).await.unwrap();
        assert!(ok.is_success());
        assert_eq!(ok.usage.unwrap().output_tokens, 2);

        let fail = up.send(req()).await.unwrap();
        assert_eq!(fail.status, 500);

        let timeout = up.send(req()).await.unwrap_err();
        assert!(matches!(timeout, AbacusError::Transport(_)));
    }
}

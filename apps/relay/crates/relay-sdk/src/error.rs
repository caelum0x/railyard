use relay_contract::{ApiErrorEnvelope, PaymentRequired, SettlementEvidence};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, RelaySdkError>;

#[derive(Debug)]
pub struct ApiFailure {
    pub status: u16,
    pub message: String,
    pub envelope: Option<ApiErrorEnvelope>,
    pub body: Vec<u8>,
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Relay returned HTTP {}: {}",
            self.status, self.message
        )
    }
}

#[derive(Debug, Error)]
pub enum RelaySdkError {
    #[error("invalid Relay SDK configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid Relay request: {0}")]
    InvalidRequest(String),
    #[error("Relay transport failed: {0}")]
    Transport(String),
    #[error("Relay response body exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("Relay returned malformed JSON: {0}")]
    InvalidJson(String),
    #[error("{0}")]
    Api(Box<ApiFailure>),
    #[error("Relay requires AITP payment")]
    PaymentRequired(Box<PaymentRequired>),
    #[error("Relay request already settled")]
    AlreadySettled(Box<SettlementEvidence>),
    #[error("Relay returned settlement evidence but no pinned Mint verifier is configured")]
    UnverifiedSettlementEvidence(Box<SettlementEvidence>),
    #[error("Relay settlement evidence failed independent Mint verification: {0}")]
    InvalidSettlementEvidence(String),
    #[error("Relay returned HTTP 402 without a valid AITP challenge: {0}")]
    InvalidPaymentChallenge(String),
    #[error("Relay SSE event exceeded {limit} bytes")]
    SseEventTooLarge { limit: usize },
    #[error("Relay SSE stream ended with an incomplete event")]
    IncompleteSseEvent,
}

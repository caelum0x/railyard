//! Official async Rust SDK for Relay.
//!
//! The SDK is deliberately independent of TensorZero implementation crates. It
//! supports JSON and multipart requests, incremental SSE, AITP-402 challenges,
//! paid retries, and authenticated settlement-evidence lookup.

mod client;
mod config;
mod error;
mod http;
mod multipart;
mod paid;
mod recovery;
mod request;
mod response;
mod sse;

pub use client::RelayClient;
pub use config::{RelayClientConfig, RelayClientConfigBuilder, RelayCredential};
pub use error::{ApiFailure, RelaySdkError, Result};
pub use http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub use multipart::{MultipartField, MultipartForm, MultipartPart};
pub use paid::AitpAuthorizer;
pub use recovery::{
    AbandonedStreamHandler, EvidenceValidationError, RecoveryPolicy, RecoveryQueue, RecoveryState,
    RecoveryWorker, SettlementEvidenceValidator,
};
pub use request::{RelayEndpoint, RequestOptions};
pub use response::{RelayResponse, RelayResponseMetadata, RelayStream};
pub use sse::{SseDecoder, SseEvent};

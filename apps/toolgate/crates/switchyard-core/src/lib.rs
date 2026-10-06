//! # switchyard-core
//!
//! The domain vocabulary every other Switchyard crate speaks:
//!
//! - [`manifest`]: tool, capability and provider manifests (loaded from YAML);
//! - [`price`]: integer micro-USD prices, quotes (hold size) and settlement;
//! - [`adapter`]: the [`adapter::Adapter`] trait, call context and error classes;
//! - [`http`]: the [`http::HttpTransport`] seam. The real implementation is the
//!   SSRF-safe client in `switchyard-egress`; tests use `switchyard-testkit`;
//! - [`error`]: the single API error envelope;
//! - [`hash`]: canonical JSON, request hashes and the audit hash chain.

pub mod adapter;
pub mod error;
pub mod hash;
pub mod http;
pub mod manifest;
pub mod price;

pub use adapter::{Acceptance, Adapter, AdapterError, AdapterOutput, CallCtx, Credential, CredentialKind, ErrorClass};
pub use error::ApiError;
pub use http::{EgressError, HttpRequest, HttpResponse, HttpTransport};
pub use manifest::{AuthMode, CapabilityManifest, OAuthSpec, ProviderKind, ProviderManifest, SideEffect, ToolManifest};
pub use price::{Price, PriceBasis};

/// A fresh ULID string with a type prefix, e.g. `call_01J...`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::new().to_string().to_lowercase())
}

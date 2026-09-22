//! Framework-agnostic MCP (Model Context Protocol) paywall helper.
//!
//! Turnstile's headline promise is "make any HTTP endpoint **or MCP tool**
//! paid-per-call". The HTTP side is [`turnstile-axum`](../../turnstile_axum);
//! this module is the MCP side, kept transport-agnostic so it drops into any MCP
//! server (a hand-rolled JSON-RPC loop, `rmcp`, or an SDK) without Turnstile
//! taking a dependency on a particular one.
//!
//! MCP tool calls are JSON-RPC 2.0. AITP-402 is carried inside that envelope:
//!
//! - the paying agent puts its already-signed `AITP-Authorization` header value
//!   into the tool arguments under the reserved key
//!   [`AITP_AUTHORIZATION_ARG`];
//! - an unpaid or invalid call is answered with a JSON-RPC **error** whose
//!   `code` is [`MCP_PAYMENT_REQUIRED`] and whose `data` carries the encoded
//!   AITP challenge ([`challenge_error`]) — the machine-readable "here is how to
//!   pay" the agent needs to retry;
//! - a verified call runs the tool and, on success, captures the actual usage;
//!   the signed receipt is attached to the tool result's `_meta` via
//!   [`receipt_meta`].
//!
//! [`McpPaywall`] wraps the same [`Turnstile`] gate the HTTP path uses, so a
//! single deployment can gate HTTP routes and MCP tools against one nonce store,
//! one settlement journal, and one live Mint — identical success-only semantics.

use std::sync::Arc;

use aitp_client::codec::{encode_challenge, encode_receipt, AITP_VERSION};
use aitp_client::{PaymentChallenge, Receipt};
use serde_json::{json, Value};

use crate::error::TurnstileError;
use crate::guard::{Admit, Turnstile};
use crate::settle::Settled;

/// Reserved MCP tool-argument key carrying the caller's already-signed
/// `AITP-Authorization` header value. Agents place their authorization here; the
/// paywall strips it before the tool sees its arguments.
pub const AITP_AUTHORIZATION_ARG: &str = "_aitp_authorization";

/// JSON-RPC error code for "payment required" — mirrors HTTP `402` in the MCP
/// envelope. Chosen in the implementation-defined server-error band and stable
/// across Turnstile so agents can branch on it.
pub const MCP_PAYMENT_REQUIRED: i64 = -32402;

/// The outcome of gating an MCP tool call. Mirrors [`Admit`] but the non-admit
/// arms are already MCP JSON-RPC error objects ready to return as the response
/// `error` field.
pub enum McpAdmit {
    /// No/absent authorization: return this JSON-RPC error (challenge in `data`).
    Challenge(Value),
    /// Verified payment: run the tool, then capture/release this hold.
    Admitted(Settled),
    /// Payment present but invalid: return this JSON-RPC error (reason + fresh
    /// challenge in `data`).
    Rejected(Value),
}

/// An MCP paywall over a shared [`Turnstile`] gate.
#[derive(Clone)]
pub struct McpPaywall {
    gate: Arc<Turnstile>,
}

impl McpPaywall {
    /// Wrap the gate that also backs the HTTP paywall (shared nonce store,
    /// settlement journal, and Mint verifier).
    #[must_use]
    pub fn new(gate: Arc<Turnstile>) -> Self {
        Self { gate }
    }

    /// The gate this paywall runs against.
    #[must_use]
    pub fn gate(&self) -> &Arc<Turnstile> {
        &self.gate
    }

    /// Gate one MCP tool call for `resource`, given the caller's authorization
    /// (its `AITP-Authorization` value pulled from the tool arguments, or `None`
    /// when absent). Non-admit outcomes come back as ready-to-send JSON-RPC
    /// error objects; an admit hands back a [`Settled`] hold to capture/release.
    pub async fn gate_call(&self, resource: &str, authorization: Option<&str>) -> McpAdmit {
        match self.gate.admit(authorization, resource).await {
            Admit::Challenge(challenge) => McpAdmit::Challenge(challenge_error(&challenge)),
            Admit::Admitted(settled) => McpAdmit::Admitted(settled),
            Admit::Rejected(error) => McpAdmit::Rejected(self.rejection_error(resource, &error).await),
        }
    }

    /// Build the JSON-RPC error for a rejected payment: the reason plus a FRESH
    /// challenge (a new single-use nonce), mirroring the HTTP re-challenge path
    /// so the agent can immediately retry with a valid authorization.
    async fn rejection_error(&self, resource: &str, error: &TurnstileError) -> Value {
        match self.gate.challenge(resource).await {
            Ok(fresh) => {
                let mut value = challenge_error(&fresh);
                if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
                    data.insert("aitp_error".into(), json!(error.to_string()));
                }
                value
            }
            // Could not mint a fresh challenge (e.g. the nonce store is down):
            // fail closed with the reason and no retry challenge.
            Err(_) => json!({
                "code": MCP_PAYMENT_REQUIRED,
                "message": "payment required",
                "data": {
                    "aitp_version": AITP_VERSION,
                    "aitp_error": error.to_string(),
                },
            }),
        }
    }
}

/// Read the reserved [`AITP_AUTHORIZATION_ARG`] out of an MCP `tools/call`
/// params object (`{ "name": ..., "arguments": { ... } }`), if present.
#[must_use]
pub fn extract_authorization(params: &Value) -> Option<String> {
    params
        .get("arguments")
        .and_then(|args| args.get(AITP_AUTHORIZATION_ARG))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Return a COPY of an MCP `tools/call` arguments object with the reserved
/// authorization key removed, so the gated tool never sees the payment token.
/// Returns an empty object when there are no arguments.
#[must_use]
pub fn tool_arguments(params: &Value) -> Value {
    match params.get("arguments") {
        Some(Value::Object(map)) => {
            let mut clean = map.clone();
            clean.remove(AITP_AUTHORIZATION_ARG);
            Value::Object(clean)
        }
        _ => json!({}),
    }
}

/// Build the JSON-RPC error object for an unpaid MCP tool call: the encoded
/// challenge plus its decoded fields under `data`, so an agent can pay without
/// re-parsing the opaque header.
#[must_use]
pub fn challenge_error(challenge: &PaymentChallenge) -> Value {
    let encoded = encode_challenge(challenge).unwrap_or_default();
    json!({
        "code": MCP_PAYMENT_REQUIRED,
        "message": "payment required",
        "data": {
            "aitp_version": AITP_VERSION,
            "aitp_challenge": encoded,
            "payee": challenge.payee,
            "amount_micros": challenge.amount_micros.get(),
            "currency": challenge.currency,
            "resource": challenge.resource,
            "nonce": challenge.nonce,
            "expiry": challenge.expiry,
            "scheme": challenge.scheme,
        },
    })
}

/// Build the `_meta` object carrying the signed settlement receipt for a
/// successful paid tool call. Attach it to the MCP tool result so the agent
/// keeps its payment evidence.
#[must_use]
pub fn receipt_meta(receipt: &Receipt) -> Value {
    let encoded = encode_receipt(receipt).unwrap_or_default();
    json!({
        "aitp_version": AITP_VERSION,
        "aitp_receipt": encoded,
        "amount_micros": receipt.amount_micros.get(),
    })
}


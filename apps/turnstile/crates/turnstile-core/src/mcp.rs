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
//! When the gate also speaks x402, tool calls follow the x402 MCP transport
//! (`specs/transports-v2/mcp.md`) next to AITP:
//!
//! - an unpaid call is answered with a tool **result** `isError: true` whose
//!   `structuredContent` is the x402 v2 `PaymentRequired` (and `content[0].text`
//!   the same JSON); the AITP challenge rides along in `_meta["aitp/challenge"]`
//!   so AITP agents can still pay the same response;
//! - the agent retries with its x402 `PaymentPayload` in
//!   `params._meta["x402/payment"]`;
//! - on success the result carries `_meta["x402/payment-response"]` (the
//!   facilitator settlement); a refused payment or failed settlement is again an
//!   `isError` result with `PaymentRequired` naming the reason.
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
use crate::x402::wire::{payment_from_json, PaymentRequiredV2};
use crate::x402::{PaymentPayload, PaymentRequirements, SettleResponse, X402Admission, X402Gate};

/// Reserved MCP tool-argument key carrying the caller's already-signed
/// `AITP-Authorization` header value. Agents place their authorization here; the
/// paywall strips it before the tool sees its arguments.
pub const AITP_AUTHORIZATION_ARG: &str = "_aitp_authorization";

/// JSON-RPC error code for "payment required" — mirrors HTTP `402` in the MCP
/// envelope. Chosen in the implementation-defined server-error band and stable
/// across Turnstile so agents can branch on it.
pub const MCP_PAYMENT_REQUIRED: i64 = -32402;

/// `params._meta` key carrying an x402 `PaymentPayload` (x402 MCP transport).
pub const X402_PAYMENT_META: &str = "x402/payment";
/// Result `_meta` key carrying the x402 `SettlementResponse`.
pub const X402_PAYMENT_RESPONSE_META: &str = "x402/payment-response";
/// Result `_meta` key carrying the AITP challenge next to an x402 `PaymentRequired`.
pub const AITP_CHALLENGE_META: &str = "aitp/challenge";

/// The x402 resource URL of an MCP tool.
#[must_use]
pub fn mcp_tool_url(tool: &str) -> String {
    format!("mcp://tool/{tool}")
}

/// A non-admitted x402 MCP call: either a tool result (`isError: true` with
/// `PaymentRequired`, per the x402 MCP transport) or a JSON-RPC error object
/// (infrastructure failure, x402 not accepted here).
#[derive(Debug, Clone, PartialEq)]
pub enum McpX402Refusal {
    ToolResult(Value),
    RpcError(Value),
}

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

    /// The x402 requirements for MCP tool `tool` (`None` when x402 is off or
    /// the tool is free).
    #[must_use]
    pub fn x402_requirements(&self, tool: &str) -> Option<PaymentRequirements> {
        self.gate.x402_requirements_for_url(&mcp_tool_url(tool))
    }

    /// The unpaid-call answer when x402 is on: an `isError` tool result with
    /// the x402 `PaymentRequired`, carrying the AITP `challenge` error's `data`
    /// in `_meta["aitp/challenge"]`. `None` when x402 is off for this tool.
    #[must_use]
    pub fn x402_payment_required(&self, tool: &str, aitp_challenge: &Value) -> Option<Value> {
        let requirements = self.x402_requirements(tool)?;
        let required = payment_required(&requirements, "payment required")?;
        Some(payment_required_result(
            &required,
            aitp_challenge.get("data"),
        ))
    }

    /// Verify an x402 payment for MCP tool `tool` and claim its nonce. On
    /// success run the tool, then [`settle_x402`](Self::settle_x402) (2xx-like
    /// outcome) or [`abandon_x402`](Self::abandon_x402) (tool failed).
    pub async fn admit_x402(
        &self,
        tool: &str,
        payment: PaymentPayload,
    ) -> Result<X402Admission, McpX402Refusal> {
        let (x402, requirements) = self.x402_rail(tool)?;
        match x402.verify_payload(payment, &requirements).await {
            Ok(admission) => Ok(admission),
            Err(rejection) if rejection.retryable => Err(McpX402Refusal::RpcError(json!({
                "code": -32603,
                "message": "x402 payment could not be verified right now",
                "data": { "x402_error": rejection.code, "message": rejection.message, "retryable": true, "charged": false },
            }))),
            Err(rejection) => Err(refused(&requirements, &rejection.code, &rejection.message)),
        }
    }

    /// Settle an admitted payment after the tool succeeded. `Ok` is the result
    /// `_meta` to attach (`x402/payment-response`); on failure the tool output
    /// must be withheld and the refusal returned instead.
    pub async fn settle_x402(
        &self,
        tool: &str,
        admission: &X402Admission,
    ) -> Result<Value, McpX402Refusal> {
        let (x402, requirements) = self.x402_rail(tool)?;
        match x402.settle(admission).await {
            Ok(settled) => Ok(settlement_meta(&settled)),
            Err(rejection) if rejection.retryable => Err(McpX402Refusal::RpcError(json!({
                "code": MCP_PAYMENT_REQUIRED,
                "message": "x402 settlement outcome unknown",
                "data": { "x402_error": rejection.code, "message": rejection.message, "x402_settlement": "unknown" },
            }))),
            Err(rejection) => Err(refused(&requirements, &rejection.code, &rejection.message)),
        }
    }

    /// The tool failed: release the claim, nothing is settled (payer not charged).
    pub async fn abandon_x402(&self, admission: &X402Admission, reason: &str) {
        if let Some(x402) = self.gate.x402() {
            x402.abandon(admission, reason).await;
        }
    }

    fn x402_rail(
        &self,
        tool: &str,
    ) -> Result<(&Arc<X402Gate>, PaymentRequirements), McpX402Refusal> {
        let not_accepted = || {
            McpX402Refusal::RpcError(json!({
                "code": -32602,
                "message": "this tool does not accept x402 payments",
                "data": { "tool": tool },
            }))
        };
        let x402 = self.gate.x402().ok_or_else(not_accepted)?;
        let requirements = self.x402_requirements(tool).ok_or_else(not_accepted)?;
        Ok((x402, requirements))
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

/// Read the x402 payment out of `params._meta["x402/payment"]`: `None` when
/// absent, `Some(Err)` when present but malformed.
#[must_use]
pub fn extract_x402_payment(params: &Value) -> Option<Result<PaymentPayload, String>> {
    params
        .get("_meta")
        .and_then(|meta| meta.get(X402_PAYMENT_META))
        .map(payment_from_json)
}

/// The x402 v2 `PaymentRequired` for one requirement.
fn payment_required(requirements: &PaymentRequirements, error: &str) -> Option<PaymentRequiredV2> {
    PaymentRequiredV2::from_v1(&X402Gate::required_body(
        vec![requirements.clone()],
        error,
    ))
}

/// The x402 MCP "payment required" tool result: `isError: true`,
/// `structuredContent` = `PaymentRequired`, `content[0].text` = the same JSON.
#[must_use]
pub fn payment_required_result(required: &PaymentRequiredV2, aitp_challenge: Option<&Value>) -> Value {
    let structured = serde_json::to_value(required).unwrap_or_else(|_| json!({}));
    let mut result = json!({
        "isError": true,
        "structuredContent": structured,
        "content": [{ "type": "text", "text": structured.to_string() }],
    });
    if let Some(challenge) = aitp_challenge {
        result["_meta"] = json!({ AITP_CHALLENGE_META: challenge });
    }
    result
}

fn refused(requirements: &PaymentRequirements, code: &str, message: &str) -> McpX402Refusal {
    match payment_required(requirements, code) {
        Some(required) => {
            let mut result = payment_required_result(&required, None);
            result["_meta"] = json!({ "x402/error": { "code": code, "message": message } });
            McpX402Refusal::ToolResult(result)
        }
        None => McpX402Refusal::RpcError(json!({
            "code": MCP_PAYMENT_REQUIRED,
            "message": message,
            "data": { "x402_error": code },
        })),
    }
}

/// The result `_meta` carrying a successful settlement.
#[must_use]
pub fn settlement_meta(settled: &SettleResponse) -> Value {
    json!({ X402_PAYMENT_RESPONSE_META: settled })
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


#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aitp_client::AmountMicros;
    use async_trait::async_trait;

    use super::*;
    use crate::config::TurnstileConfig;
    use crate::nonce::InMemoryNonceStore;
    use crate::verifier::FakeVerifier;
    use crate::x402::{
        config::USDC_BASE_SEPOLIA, Facilitator, InMemoryX402Journal, VerifyResponse, X402Config,
    };
    use crate::ChallengeIssuer;

    const PAY_TO: &str = "0x2222222222222222222222222222222222222222";
    const PAYER: &str = "0x1111111111111111111111111111111111111111";

    #[derive(Default)]
    struct Facil {
        settle_fails: bool,
        calls: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl Facilitator for Facil {
        async fn verify(
            &self,
            _: &PaymentPayload,
            _: &PaymentRequirements,
        ) -> Result<VerifyResponse, String> {
            self.calls.lock().unwrap().push("verify");
            Ok(VerifyResponse { is_valid: true, invalid_reason: None, payer: Some(PAYER.into()) })
        }

        async fn settle(
            &self,
            _: &PaymentPayload,
            r: &PaymentRequirements,
        ) -> Result<SettleResponse, String> {
            self.calls.lock().unwrap().push("settle");
            Ok(SettleResponse {
                success: !self.settle_fails,
                error_reason: self.settle_fails.then(|| "insufficient_funds".to_string()),
                transaction: if self.settle_fails { String::new() } else { "0xtx".into() },
                network: crate::x402::wire::caip2_network(&r.network),
                payer: Some(PAYER.into()),
            })
        }
    }

    fn paywall(facil: Arc<Facil>, x402: bool) -> McpPaywall {
        let config = TurnstileConfig {
            mint_server_url: "http://mint.internal".into(),
            api_key: "k".into(),
            payee: "ait1qpayee".into(),
            price_micros: AmountMicros(5_000),
            currency: "AIT".into(),
            challenge_ttl_ms: 60_000,
        };
        let nonces = Arc::new(InMemoryNonceStore::new());
        let gate = Turnstile::new(
            ChallengeIssuer::new(config, nonces.clone()),
            Arc::new(FakeVerifier::with_hold("hold_1")),
            nonces,
        );
        let gate = if x402 {
            let rail = X402Gate::new(
                X402Config {
                    pay_to: PAY_TO.into(),
                    network: "base-sepolia".into(),
                    facilitator_url: "https://x402.org/facilitator".into(),
                    facilitator_auth: None,
                    cdp_key: None,
                    asset: USDC_BASE_SEPOLIA.into(),
                    asset_name: "USD Coin".into(),
                    asset_version: "2".into(),
                    max_timeout_seconds: 60,
                    mint_account: None,
                    public_url: None,
                },
                facil,
                Arc::new(InMemoryX402Journal::new()),
                None,
            );
            gate.with_x402(Arc::new(rail), "Search the web")
        } else {
            gate
        };
        McpPaywall::new(Arc::new(gate))
    }

    fn payment(paywall: &McpPaywall, nonce: &str) -> Value {
        let accepted = paywall.x402_requirements("search").unwrap().to_v2();
        json!({
            "x402Version": 2,
            "resource": { "url": "mcp://tool/search" },
            "accepted": accepted,
            "payload": {
                "signature": "0xsig",
                "authorization": {
                    "from": PAYER, "to": PAY_TO, "value": "5000",
                    "validAfter": "0", "validBefore": "99999999999", "nonce": nonce
                }
            }
        })
    }

    #[tokio::test]
    async fn unpaid_call_is_an_x402_payment_required_result_with_the_aitp_challenge() {
        let paywall = paywall(Arc::new(Facil::default()), true);
        let McpAdmit::Challenge(aitp) = paywall.gate_call("tools/call#search", None).await else {
            panic!("expected a challenge");
        };
        let result = paywall.x402_payment_required("search", &aitp).unwrap();
        assert_eq!(result["isError"], true);
        let structured = &result["structuredContent"];
        assert_eq!(structured["x402Version"], 2);
        assert_eq!(structured["resource"]["url"], "mcp://tool/search");
        assert_eq!(structured["resource"]["description"], "Search the web");
        assert_eq!(structured["accepts"][0]["amount"], "5000");
        assert_eq!(structured["accepts"][0]["network"], "eip155:84532");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(&text, structured);
        assert!(result["_meta"][AITP_CHALLENGE_META]["aitp_challenge"].is_string());
        // x402 off: no x402 result, the AITP JSON-RPC error stands.
        let plain = self::paywall(Arc::new(Facil::default()), false);
        assert!(plain.x402_payment_required("search", &aitp).is_none());
    }

    #[tokio::test]
    async fn meta_payment_is_verified_settled_and_replay_refused() {
        let facil = Arc::new(Facil::default());
        let paywall = paywall(facil.clone(), true);
        let params = json!({
            "name": "search",
            "arguments": { "q": "x" },
            "_meta": { X402_PAYMENT_META: payment(&paywall, "0x01") },
        });
        let payload = extract_x402_payment(&params).unwrap().unwrap();
        let admission = paywall.admit_x402("search", payload.clone()).await.unwrap();
        let meta = paywall.settle_x402("search", &admission).await.unwrap();
        assert_eq!(meta[X402_PAYMENT_RESPONSE_META]["transaction"], "0xtx");
        assert_eq!(meta[X402_PAYMENT_RESPONSE_META]["network"], "eip155:84532");
        assert_eq!(*facil.calls.lock().unwrap(), vec!["verify", "settle"]);

        let McpX402Refusal::ToolResult(replay) =
            paywall.admit_x402("search", payload).await.unwrap_err()
        else {
            panic!("replay should be a PaymentRequired tool result");
        };
        assert_eq!(replay["isError"], true);
        assert_eq!(
            replay["structuredContent"]["error"],
            "invalid_exact_evm_payload_authorization_nonce_used"
        );
    }

    #[tokio::test]
    async fn failed_settlement_withholds_output_as_payment_required() {
        let facil = Arc::new(Facil { settle_fails: true, ..Facil::default() });
        let paywall = paywall(facil, true);
        let payload = payment_from_json(&payment(&paywall, "0x02")).unwrap();
        let admission = paywall.admit_x402("search", payload).await.unwrap();
        let McpX402Refusal::ToolResult(result) =
            paywall.settle_x402("search", &admission).await.unwrap_err()
        else {
            panic!("expected a tool result");
        };
        assert_eq!(result["structuredContent"]["error"], "insufficient_funds");
        assert!(result["structuredContent"]["accepts"].is_array());
    }

    #[tokio::test]
    async fn x402_payment_on_an_aitp_only_gate_is_an_invalid_params_error() {
        let paywall = paywall(Arc::new(Facil::default()), false);
        let payload = PaymentPayload::v1("exact", "base-sepolia", json!({}));
        let McpX402Refusal::RpcError(error) =
            paywall.admit_x402("search", payload).await.unwrap_err()
        else {
            panic!("expected a JSON-RPC error");
        };
        assert_eq!(error["code"], -32602);
    }

    #[test]
    fn malformed_meta_payment_is_reported() {
        let params = json!({ "_meta": { X402_PAYMENT_META: { "x402Version": 2 } } });
        assert!(extract_x402_payment(&params).unwrap().is_err());
        assert!(extract_x402_payment(&json!({ "arguments": {} })).is_none());
    }
}

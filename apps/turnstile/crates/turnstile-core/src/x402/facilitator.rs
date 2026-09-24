//! x402 facilitator client (`POST /verify`, `POST /settle`) and the Mint
//! deposit poster that credits settled USDC into the payee's Mint account.
//!
//! Both talk HTTP through the same injectable
//! [`HttpTransport`](aitp_client::transport::HttpTransport) seam the AITP rail
//! uses, so tests drive them with an in-process fake and production uses
//! [`ReqwestTransport`](aitp_client::ReqwestTransport) bound to the configured
//! base URL.

use std::sync::Arc;

use aitp_client::transport::{HttpRequest, HttpTransport};
use async_trait::async_trait;
use serde_json::json;

use super::cdp::CdpFacilitatorAuth;
use super::config::X402Config;
use super::wire::{
    FacilitatorRequest, PaymentPayload, PaymentRequirements, SettleResponse, VerifyResponse,
    X402_VERSION, X402_VERSION_V2,
};

/// Verifies and settles x402 payments.
#[async_trait]
pub trait Facilitator: Send + Sync {
    async fn verify(
        &self,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<VerifyResponse, String>;

    async fn settle(
        &self,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<SettleResponse, String>;
}

/// How requests to the facilitator authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FacilitatorAuth {
    /// Keyless (x402.org testnet facilitator).
    None,
    /// A static `Authorization` header value.
    Static(String),
    /// Coinbase CDP: a fresh ES256/EdDSA JWT per request.
    Cdp(Box<CdpFacilitatorAuth>),
}

impl FacilitatorAuth {
    /// A short label for logs (never includes secrets).
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::Static(_) => "static Authorization header".into(),
            Self::Cdp(cdp) => format!(
                "Coinbase CDP JWT ({}, key {})",
                cdp.key().algorithm(),
                cdp.key().key_id()
            ),
        }
    }

    fn headers(&self, method: &str, operation: &str) -> Vec<(String, String)> {
        match self {
            Self::None => Vec::new(),
            Self::Static(value) => vec![("authorization".to_string(), value.clone())],
            Self::Cdp(cdp) => cdp.headers(method, operation),
        }
    }
}

/// A facilitator reached over HTTP (x402.org, Coinbase CDP, PayAI, self-hosted).
pub struct HttpFacilitator {
    transport: Arc<dyn HttpTransport>,
    auth: FacilitatorAuth,
}

impl HttpFacilitator {
    /// `transport` must be bound to the facilitator base URL.
    /// `authorization` is sent verbatim as the `Authorization` header.
    pub fn new(transport: Arc<dyn HttpTransport>, authorization: Option<String>) -> Self {
        Self::with_auth(
            transport,
            authorization.map_or(FacilitatorAuth::None, FacilitatorAuth::Static),
        )
    }

    /// `transport` must be bound to the facilitator base URL.
    pub fn with_auth(transport: Arc<dyn HttpTransport>, auth: FacilitatorAuth) -> Self {
        Self { transport, auth }
    }

    /// The facilitator `config` names: CDP JWT auth when a CDP key is set,
    /// else the static header, else keyless. `transport` must be bound to
    /// `config.facilitator_url`.
    pub fn from_config(
        transport: Arc<dyn HttpTransport>,
        config: &X402Config,
    ) -> Result<Self, crate::error::TurnstileError> {
        let auth = match (&config.cdp_key, &config.facilitator_auth) {
            (Some(key), _) => FacilitatorAuth::Cdp(Box::new(
                CdpFacilitatorAuth::new(key.clone(), &config.facilitator_url)
                    .map_err(crate::error::TurnstileError::Config)?,
            )),
            (None, Some(value)) => FacilitatorAuth::Static(value.clone()),
            (None, None) => FacilitatorAuth::None,
        };
        Ok(Self::with_auth(transport, auth))
    }

    #[must_use]
    pub fn auth(&self) -> &FacilitatorAuth {
        &self.auth
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<T, String> {
        // The facilitator validates the requirement in the payload's own
        // version: v2 payments are checked against v2 (CAIP-2) requirements.
        let body = if payload.x402_version >= X402_VERSION_V2 {
            serde_json::to_vec(&FacilitatorRequest {
                x402_version: X402_VERSION_V2,
                payment_payload: payload,
                payment_requirements: &requirements.to_v2(),
            })
        } else {
            serde_json::to_vec(&FacilitatorRequest {
                x402_version: X402_VERSION,
                payment_payload: payload,
                payment_requirements: requirements,
            })
        }
        .map_err(|e| e.to_string())?;
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        headers.extend(self.auth.headers("POST", path));
        let response = self
            .transport
            .send(HttpRequest {
                method: "POST",
                path: path.to_string(),
                headers,
                body,
            })
            .await
            .map_err(|e| format!("facilitator {path} unreachable: {e}"))?;
        // Facilitators answer `400` with a well-formed `isValid:false` /
        // `success:false` body for a rejected payment, so parse any status and
        // only fail when the body is not the expected shape.
        serde_json::from_slice::<T>(&response.body).map_err(|e| {
            let snippet: String = String::from_utf8_lossy(&response.body)
                .chars()
                .take(200)
                .collect();
            format!(
                "facilitator {path} returned {} with an unexpected body ({e}): {snippet}",
                response.status
            )
        })
    }
}

#[async_trait]
impl Facilitator for HttpFacilitator {
    async fn verify(
        &self,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<VerifyResponse, String> {
        self.post("/verify", payload, requirements).await
    }

    async fn settle(
        &self,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<SettleResponse, String> {
        self.post("/settle", payload, requirements).await
    }
}

/// Outcome of crediting one settled payment into Mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintCredit {
    /// Mint confirmed the on-chain deposit and credited the account.
    Credited,
    /// Mint could not confirm yet (chain not final, RPC down): retry later.
    Retry(String),
    /// Mint rejected the credit for a reason a retry will not fix.
    Failed(String),
}

/// Credits settled x402 payments into Mint via `POST /v1/mint`.
///
/// The deposit reference is `usdc:<tx hash>`, so Mint's USDC ramp independently
/// re-reads the transaction receipt on-chain and decodes the ERC-20 `Transfer`
/// to its treasury before minting. Mint keys idempotency on the deposit ref, so
/// replays never double-credit. For the credit to confirm, the x402 `payTo`
/// must be the Mint USDC treasury (`deposit_address` of Mint's `usdc` provider).
pub struct MintDepositPoster {
    transport: Arc<dyn HttpTransport>,
    api_key: String,
    account: String,
}

impl MintDepositPoster {
    pub fn new(transport: Arc<dyn HttpTransport>, api_key: String, account: String) -> Self {
        Self {
            transport,
            api_key,
            account,
        }
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Credit `amount_micros` (USDC atomic units == microdollars) for `tx`.
    pub async fn credit(&self, tx: &str, amount_micros: i64) -> MintCredit {
        let body = json!({
            "to": self.account,
            "amount_micros": amount_micros,
            "deposit_ref": format!("usdc:{tx}"),
            "idem_key": format!("x402:{tx}"),
        });
        let request = HttpRequest {
            method: "POST",
            path: "/v1/mint".to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), self.api_key.clone()),
            ],
            body: body.to_string().into_bytes(),
        };
        match self.transport.send(request).await {
            Err(e) => MintCredit::Retry(format!("mint unreachable: {e}")),
            Ok(response) => {
                let text: String = String::from_utf8_lossy(&response.body)
                    .chars()
                    .take(300)
                    .collect();
                match response.status {
                    200..=299 => MintCredit::Credited,
                    // 402 = Mint's USDC ramp cannot confirm the tx yet (receipt
                    // not found / not final / RPC unavailable); 5xx transient.
                    // The reaper bounds retries, so a permanently unconfirmable
                    // deposit (wrong treasury) ends as failed, not a hot loop.
                    402 | 408 | 425 | 429 | 500..=599 => {
                        MintCredit::Retry(format!("mint {}: {text}", response.status))
                    }
                    status => MintCredit::Failed(format!("mint {status}: {text}")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aitp_client::transport::HttpResponse;
    use aitp_client::PayError;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use p256::ecdsa::signature::Verifier as _;
    use serde_json::{json, Value};

    use super::*;
    use crate::x402::cdp::{tests::ec_pem, CdpApiKey, CDP_FACILITATOR_URL};
    use crate::x402::wire::PaymentRequirementsV2;

    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<HttpRequest>>,
    }

    #[async_trait]
    impl HttpTransport for Recorder {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
            let body = if req.path.ends_with("/verify") {
                json!({ "isValid": true, "payer": "0x11" })
            } else {
                json!({ "success": true, "transaction": "0xtx", "network": "eip155:84532" })
            };
            self.requests.lock().unwrap().push(req);
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: body.to_string().into_bytes(),
            })
        }
    }

    fn requirements() -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: "base-sepolia".into(),
            max_amount_required: "5000".into(),
            resource: "https://t.example/v1/echo".into(),
            description: "Echo".into(),
            mime_type: "application/json".into(),
            pay_to: "0x2222222222222222222222222222222222222222".into(),
            max_timeout_seconds: 60,
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            output_schema: None,
            extra: None,
        }
    }

    fn v2_payload(req: &PaymentRequirements) -> PaymentPayload {
        PaymentPayload {
            x402_version: 2,
            accepted: Some(req.to_v2()),
            payload: json!({ "signature": "0xsig", "authorization": {} }),
            ..PaymentPayload::default()
        }
    }

    fn cdp_config() -> X402Config {
        X402Config {
            pay_to: "0x2222222222222222222222222222222222222222".into(),
            network: "base-sepolia".into(),
            facilitator_url: CDP_FACILITATOR_URL.into(),
            facilitator_auth: None,
            cdp_key: Some(CdpApiKey::parse("organizations/o/apiKeys/k", &ec_pem()).unwrap()),
            asset: crate::x402::config::USDC_BASE_SEPOLIA.into(),
            asset_name: "USD Coin".into(),
            asset_version: "2".into(),
            max_timeout_seconds: 60,
            mint_account: None,
            public_url: None,
        }
    }

    fn header<'a>(req: &'a HttpRequest, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    #[tokio::test]
    async fn cdp_requests_carry_a_signed_jwt_bound_to_each_operation() {
        let recorder = Arc::new(Recorder::default());
        let config = cdp_config();
        let facilitator = HttpFacilitator::from_config(recorder.clone(), &config).unwrap();
        assert!(facilitator.auth().describe().contains("ES256"));
        let req = requirements();
        let payload = v2_payload(&req);
        facilitator.verify(&payload, &req).await.unwrap();
        facilitator.settle(&payload, &req).await.unwrap();

        let key = config.cdp_key.unwrap();
        let verifying =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&key.public_key_bytes()).unwrap();
        let sent = recorder.requests.lock().unwrap();
        for (request, op) in sent.iter().zip(["verify", "settle"]) {
            assert_eq!(request.path, format!("/{op}"));
            let jwt = header(request, "authorization")
                .and_then(|v| v.strip_prefix("Bearer "))
                .expect("bearer JWT");
            let parts: Vec<&str> = jwt.split('.').collect();
            let claims: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
            assert_eq!(
                claims["uris"][0],
                format!("POST api.cdp.coinbase.com/platform/v2/x402/{op}")
            );
            assert_eq!(claims["sub"], "organizations/o/apiKeys/k");
            let signature = p256::ecdsa::Signature::from_slice(
                &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            )
            .unwrap();
            verifying
                .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
                .expect("JWT signature verifies against the CDP key");
            assert!(header(request, "correlation-context").is_some());
        }
    }

    #[tokio::test]
    async fn v2_payments_are_forwarded_with_v2_requirements() {
        let recorder = Arc::new(Recorder::default());
        let facilitator = HttpFacilitator::new(recorder.clone(), None);
        let req = requirements();
        facilitator.verify(&v2_payload(&req), &req).await.unwrap();
        let sent = recorder.requests.lock().unwrap();
        assert!(header(&sent[0], "authorization").is_none());
        let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(body["x402Version"], 2);
        let forwarded: PaymentRequirementsV2 =
            serde_json::from_value(body["paymentRequirements"].clone()).unwrap();
        assert_eq!(forwarded.network, "eip155:84532");
        assert_eq!(forwarded.amount, "5000");
        assert_eq!(body["paymentPayload"]["accepted"]["amount"], "5000");
    }

    #[tokio::test]
    async fn v1_payments_keep_v1_requirements_and_static_auth() {
        let recorder = Arc::new(Recorder::default());
        let facilitator = HttpFacilitator::new(recorder.clone(), Some("Bearer static".into()));
        let req = requirements();
        let payload = PaymentPayload::v1("exact", "base-sepolia", json!({}));
        facilitator.verify(&payload, &req).await.unwrap();
        let sent = recorder.requests.lock().unwrap();
        assert_eq!(header(&sent[0], "authorization"), Some("Bearer static"));
        let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(body["x402Version"], 1);
        assert_eq!(body["paymentRequirements"]["maxAmountRequired"], "5000");
        assert_eq!(body["paymentRequirements"]["network"], "base-sepolia");
    }
}

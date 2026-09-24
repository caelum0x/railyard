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

use super::wire::{
    FacilitatorRequest, PaymentPayload, PaymentRequirements, SettleResponse, VerifyResponse,
    X402_VERSION,
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

/// A facilitator reached over HTTP (x402.org, Coinbase CDP, PayAI, self-hosted).
pub struct HttpFacilitator {
    transport: Arc<dyn HttpTransport>,
    authorization: Option<String>,
}

impl HttpFacilitator {
    /// `transport` must be bound to the facilitator base URL.
    /// `authorization` is sent verbatim as the `Authorization` header.
    pub fn new(transport: Arc<dyn HttpTransport>, authorization: Option<String>) -> Self {
        Self {
            transport,
            authorization,
        }
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<T, String> {
        let body = serde_json::to_vec(&FacilitatorRequest {
            x402_version: payload.x402_version.max(X402_VERSION),
            payment_payload: payload,
            payment_requirements: requirements,
        })
        .map_err(|e| e.to_string())?;
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if let Some(auth) = &self.authorization {
            headers.push(("authorization".to_string(), auth.clone()));
        }
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

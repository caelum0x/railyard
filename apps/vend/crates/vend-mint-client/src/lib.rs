//! # vend-mint-client — real HTTP client to the live mint-server.
//!
//! Implements [`MintTransport`] over `reqwest`. Authenticates with `X-API-Key`,
//! sends Mint's exact `from`/`to` hold DTOs with integer `amount_micros`, and
//! maps `{success,error.code}` bodies onto
//! [`VendError`]: 402 → short-circuit (`InsufficientFunds`/`ReserveInsufficient`),
//! 409 → `IdempotencyConflict`, 422 → `CaptureExceedsHold`.

use async_trait::async_trait;
use serde::Deserialize;
use std::fmt;
use std::time::Duration;

use vend_core::error::VendError;
use vend_core::transport::{
    CaptureRequest, CaptureResponse, HoldRequest, HoldResponse, HoldView, MintTransport,
    ReleaseRequest, ReleaseResponse, TransferRequest, TransferResponse,
};

/// A configured client for one mint-server deployment.
#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

/// Network deadlines and connection-pool bounds for the Mint dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintClientConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub read_timeout: Duration,
    pub pool_idle_timeout: Duration,
    pub max_idle_per_host: usize,
}

impl Default for MintClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(30),
            max_idle_per_host: 16,
        }
    }
}

/// Invalid URL or timeout configuration. It is intentionally secret-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintClientConfigError(String);

impl fmt::Display for MintClientConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MintClientConfigError {}

/// The `{success:false,error:{code,message}}` shape returned by mint-server.
#[derive(Debug, Deserialize)]
struct MintErrorBody {
    error: MintErrorInner,
}

#[derive(Debug, Deserialize)]
struct MintErrorInner {
    code: String,
    #[serde(default)]
    message: String,
}

impl MintClient {
    /// Build a client from a base URL (e.g. `https://mint.internal`) and the
    /// shared `X-API-Key` secret.
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_config(base, api_key, MintClientConfig::default())
            .expect("default Mint HTTP client configuration must be valid")
    }

    /// Build a hardened client with explicit network deadlines.
    pub fn with_config(
        base: impl Into<String>,
        api_key: impl Into<String>,
        config: MintClientConfig,
    ) -> Result<Self, MintClientConfigError> {
        validate_config(&config)?;
        let base = normalize_base(base.into())?;
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .read_timeout(config.read_timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.max_idle_per_host)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("vend-mint-client/0.1")
            .build()
            .map_err(|error| MintClientConfigError(format!("build Mint HTTP client: {error}")))?;
        Ok(MintClient {
            http,
            base,
            api_key: api_key.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Send a JSON POST and decode either the success `data` payload or map the
    /// error body/status onto a [`VendError`].
    async fn post_json<B: serde::Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, VendError> {
        let resp = self
            .http
            .post(self.url(path))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| VendError::Transport(e.to_string()))?;
        Self::decode(resp).await
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, VendError> {
        let resp = self
            .http
            .get(self.url(path))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| VendError::Transport(e.to_string()))?;
        Self::decode(resp).await
    }

    async fn decode<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T, VendError> {
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| VendError::Transport(e.to_string()))?;

        if (200..300).contains(&status) {
            // Success envelope: {"success":true,"data":<T>}.
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| VendError::Transport(format!("decode success: {e}")))?;
            let data = value.get("data").cloned().unwrap_or(value);
            return serde_json::from_value(data)
                .map_err(|e| VendError::Transport(format!("decode data: {e}")));
        }

        // Error path: map by status first, then by stable code.
        let code = serde_json::from_str::<MintErrorBody>(&text)
            .map(|b| (b.error.code, b.error.message))
            .unwrap_or_else(|_| (String::new(), text.clone()));
        Err(map_error(status, &code.0, code.1))
    }
}

fn normalize_base(base: String) -> Result<String, MintClientConfigError> {
    let base = base.trim_end_matches('/');
    let parsed = reqwest::Url::parse(base)
        .map_err(|_| MintClientConfigError("Mint base URL must be an absolute URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(MintClientConfigError(
            "Mint base URL must use http or https and include a host".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MintClientConfigError(
            "Mint base URL must not contain credentials".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(MintClientConfigError(
            "Mint base URL must not contain a query or fragment".into(),
        ));
    }
    Ok(base.to_string())
}

fn validate_config(config: &MintClientConfig) -> Result<(), MintClientConfigError> {
    if config.connect_timeout.is_zero()
        || config.request_timeout.is_zero()
        || config.read_timeout.is_zero()
        || config.pool_idle_timeout.is_zero()
        || config.max_idle_per_host == 0
    {
        return Err(MintClientConfigError(
            "Mint HTTP timeouts and pool limit must be greater than zero".into(),
        ));
    }
    Ok(())
}

/// Map a mint-server error `(status, code, message)` onto a [`VendError`].
fn map_error(status: u16, code: &str, message: String) -> VendError {
    match (status, code) {
        (402, "reserve_insufficient") => VendError::ReserveInsufficient,
        (402, _) => VendError::InsufficientFunds,
        (409, _) => VendError::IdempotencyConflict,
        (422, _) => VendError::CaptureExceedsHold,
        (401, _) | (403, _) => VendError::Unauthorized,
        (404, _) => VendError::NotFound,
        _ => VendError::Upstream {
            status,
            code: code.to_string(),
            message,
        },
    }
}

#[async_trait]
impl MintTransport for MintClient {
    async fn create_hold(&self, req: HoldRequest) -> Result<HoldResponse, VendError> {
        self.post_json("/v1/holds", &req).await
    }

    async fn get_hold(&self, hold_id: &str) -> Result<HoldView, VendError> {
        self.get_json(&format!("/v1/holds/{hold_id}")).await
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        req: CaptureRequest,
    ) -> Result<CaptureResponse, VendError> {
        self.post_json(&format!("/v1/holds/{hold_id}/capture"), &req)
            .await
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        req: ReleaseRequest,
    ) -> Result<ReleaseResponse, VendError> {
        self.post_json(&format!("/v1/holds/{hold_id}/release"), &req)
            .await
    }

    async fn transfer(&self, req: TransferRequest) -> Result<TransferResponse, VendError> {
        self.post_json("/v1/transfers", &req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardened_config_rejects_unsafe_or_unbounded_values() {
        assert!(
            MintClient::with_config("file:///tmp/mint", "secret", MintClientConfig::default())
                .is_err()
        );
        assert!(MintClient::with_config(
            "https://user:password@mint.internal",
            "secret",
            MintClientConfig::default()
        )
        .is_err());
        let invalid = MintClientConfig {
            request_timeout: Duration::ZERO,
            ..MintClientConfig::default()
        };
        assert!(MintClient::with_config("https://mint.internal", "secret", invalid).is_err());
    }

    #[tokio::test]
    async fn total_deadline_stops_a_stalled_mint_operation() {
        use axum::routing::post;
        use axum::Router;

        let app = Router::new().route(
            "/v1/holds",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                axum::Json(serde_json::json!({
                    "success": true,
                    "data": {"hold_id": "hold_too_late"}
                }))
            }),
        );
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind timeout test server: {error}"),
        };
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = MintClient::with_config(
            base,
            "secret",
            MintClientConfig {
                request_timeout: Duration::from_millis(20),
                ..MintClientConfig::default()
            },
        )
        .unwrap();
        let error = client
            .create_hold(HoldRequest {
                from: "acct_customer".into(),
                amount_micros: 100,
                idem_key: "timeout:hold".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, VendError::Transport(_)));
    }

    async fn send_real_mint<B, T>(
        app: &axum::Router,
        method: &str,
        path: &str,
        body: Option<&B>,
    ) -> T
    where
        B: serde::Serialize,
        T: serde::de::DeserializeOwned,
    {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-api-key", "vend-mint-contract-test")
            .header("content-type", "application/json")
            .body(Body::from(
                body.map(serde_json::to_vec)
                    .transpose()
                    .unwrap()
                    .unwrap_or_default(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        serde_json::from_value(value["data"].clone()).unwrap()
    }

    async fn post_real_mint<B, T>(app: &axum::Router, path: &str, body: &B) -> T
    where
        B: serde::Serialize,
        T: serde::de::DeserializeOwned,
    {
        send_real_mint(app, "POST", path, Some(body)).await
    }

    #[test]
    fn maps_status_codes_to_short_circuits() {
        assert_eq!(
            map_error(402, "x", String::new()),
            VendError::InsufficientFunds
        );
        assert_eq!(
            map_error(402, "reserve_insufficient", String::new()),
            VendError::ReserveInsufficient
        );
        assert_eq!(
            map_error(409, "x", String::new()),
            VendError::IdempotencyConflict
        );
        assert_eq!(
            map_error(422, "x", String::new()),
            VendError::CaptureExceedsHold
        );
    }

    /// Exercise the production Vend HTTP adapter against Mint's actual router,
    /// handlers, DTOs, auth middleware, and in-memory ledger implementation.
    /// This catches wire drift that Vend's FakeMint cannot detect.
    #[tokio::test]
    async fn vend_dtos_match_real_mint_router_hold_contract() {
        use std::sync::Arc;

        use mint_core::{AccountKind, Micros};
        use mint_protocol::InMemoryPayerKeys;
        use mint_reserve::{FakeKycGate, FakeRamp, KycTier};
        use mint_server::{build_router, AppState};
        use mint_store::{Clock, FixedClock, InMemoryStore, LedgerStore};

        const API_KEY: &str = "vend-mint-contract-test";
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(1_000_000));
        let store = InMemoryStore::with_clock(clock.clone());
        let payer = store.create_account(AccountKind::User, None).await.unwrap();
        store
            .seed_account(&payer, Micros::new(10_000))
            .await
            .unwrap();
        let payee = store
            .create_account(AccountKind::Model, None)
            .await
            .unwrap();
        let state = AppState::new(
            store,
            clock,
            Arc::new(FakeRamp::new()),
            Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX)),
            Arc::new(InMemoryPayerKeys::new()),
            API_KEY,
            "whsec_test",
        );
        let app = build_router(state);
        let captured: HoldResponse = post_real_mint(
            &app,
            "/v1/holds",
            &HoldRequest {
                from: payer.as_str().to_string(),
                amount_micros: 4_000,
                idem_key: "contract:capture:hold".to_string(),
            },
        )
        .await;
        let receipt: CaptureResponse = post_real_mint(
            &app,
            &format!("/v1/holds/{}/capture", captured.hold_id),
            &CaptureRequest {
                amount_micros: 1_500,
                to: payee.as_str().to_string(),
                idem_key: "contract:capture:capture".to_string(),
            },
        )
        .await;
        assert_eq!(receipt.captured_micros, 1_500);

        let view: HoldView = send_real_mint::<(), _>(
            &app,
            "GET",
            &format!("/v1/holds/{}", captured.hold_id),
            None,
        )
        .await;
        assert_eq!(view.status, vend_core::transport::HoldStatus::Captured);
        assert_eq!(view.captured_micros, 1_500);

        let transfer: TransferResponse = post_real_mint(
            &app,
            "/v1/transfers",
            &TransferRequest {
                from: payee.as_str().to_string(),
                to: payer.as_str().to_string(),
                amount_micros: 500,
                idem_key: "contract:payout:provider".to_string(),
                ref_: Some("vend:contract:provider".to_string()),
            },
        )
        .await;
        assert!(!transfer.idempotent_replay);
        let replay: TransferResponse = post_real_mint(
            &app,
            "/v1/transfers",
            &TransferRequest {
                from: payee.as_str().to_string(),
                to: payer.as_str().to_string(),
                amount_micros: 500,
                idem_key: "contract:payout:provider".to_string(),
                ref_: Some("vend:contract:provider".to_string()),
            },
        )
        .await;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.first_seq, transfer.first_seq);

        let released: HoldResponse = post_real_mint(
            &app,
            "/v1/holds",
            &HoldRequest {
                from: payer.as_str().to_string(),
                amount_micros: 1_000,
                idem_key: "contract:release:hold".to_string(),
            },
        )
        .await;
        let _: ReleaseResponse = post_real_mint(
            &app,
            &format!("/v1/holds/{}/release", released.hold_id),
            &ReleaseRequest {
                idem_key: "contract:release:release".to_string(),
            },
        )
        .await;
    }
}

//! Server-side verification: forward an agent's already-signed authorization to
//! a live mint-server and settle the resulting hold.
//!
//! This is the boundary the task pins: `aitp_client::MintPaymentGateway` models
//! the CALLER (it re-signs), so Turnstile canNOT reuse it. The verifier here is
//! the SERVER counterpart — it takes the decoded [`PaymentAuthorization`] the
//! agent sent and forwards it verbatim to the mint over the reused
//! [`HttpTransport`] seam:
//!
//! - [`verify`](PaymentVerifier::verify) → `POST /v1/aitp/verify` (mint runs
//!   `verify_and_settle`; an `aitp-hold` challenge yields a `hold_id`);
//! - [`capture`](PaymentVerifier::capture) → `POST /v1/holds/:id/capture` (settle
//!   ACTUAL usage to the payee — success path);
//! - [`release`](PaymentVerifier::release) → `POST /v1/holds/:id/release` (free
//!   the reservation — failure path, the agent pays nothing, SPEC ADR-0002).
//!
//! The mint's capture route needs the payee's `AccountId`; the verifier resolves
//! the configured `ait1…` payee address once via `GET /v1/addresses/:address`
//! and caches it. The transport is injectable so tests drive [`MintVerifier`]
//! with a fake, and [`FakeVerifier`] short-circuits the mint entirely for pure
//! unit tests of the guard/settle layers.

use std::sync::{Arc, Mutex};

use aitp_client::codec::{decode_receipt, HEADER_RECEIPT};
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, HoldRef, PayError, PaymentAuthorization, Receipt, Scheme};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::TurnstileError;

/// Header carrying the per-request API key on every protected mint route.
const HEADER_API_KEY: &str = "x-api-key";

/// The outcome of a server-side verify: for `aitp-hold`, a [`HoldRef`] to later
/// capture/release; for `aitp-transfer`, `None` (already final).
#[derive(Debug, Clone)]
pub struct VerifiedHold {
    pub hold: Option<HoldRef>,
    pub scheme: Scheme,
}

/// Server-side AITP-402 verifier. Turnstile forwards the agent's already-signed
/// authorization here; implementors never re-sign.
#[async_trait]
pub trait PaymentVerifier: Send + Sync {
    /// Forward `auth` to the settlement rail for server-side verify + settle.
    async fn verify(&self, auth: &PaymentAuthorization) -> Result<VerifiedHold, TurnstileError>;

    /// Capture `actual` against a hold — the SUCCESS path. Returns Mint's exact
    /// signed, hash-linked settlement receipt.
    async fn capture(
        &self,
        hold: &HoldRef,
        actual: AmountMicros,
    ) -> Result<Receipt, TurnstileError>;

    /// Release a hold without settling — the FAILURE path (agent pays nothing).
    async fn release(&self, hold: &HoldRef) -> Result<(), TurnstileError>;
}

/// The production [`PaymentVerifier`]: forwards over an injected transport to a
/// live mint-server.
pub struct MintVerifier {
    transport: Arc<dyn HttpTransport>,
    api_key: String,
    /// Provider receive address (`ait1…`) whose `AccountId` capture settles to.
    payee_address: String,
    /// Cached resolution of `payee_address` → `AccountId`.
    payee_account: Mutex<Option<String>>,
}

impl MintVerifier {
    /// Assemble a verifier over an injected transport. `api_key` is the
    /// mint-server key; `payee_address` is this provider's `ait1…` receive
    /// address (resolved to a capture `to` on first settle).
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        api_key: impl Into<String>,
        payee_address: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            payee_address: payee_address.into(),
            payee_account: Mutex::new(None),
        }
    }

    /// Send a request with the API key attached, mapping non-2xx onto a typed
    /// [`TurnstileError`] via the mint error envelope.
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TurnstileError> {
        let req = req.with_header(HEADER_API_KEY, &self.api_key);
        let resp = self
            .transport
            .send(req)
            .await
            .map_err(TurnstileError::Pay)?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(map_error_response(&resp))
        }
    }

    /// Resolve (and cache) the payee `ait1…` address to its `AccountId`.
    async fn payee_account_id(&self) -> Result<String, TurnstileError> {
        {
            let cached = self.payee_account.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(acct) = cached.clone() {
                return Ok(acct);
            }
        }
        let resp = self
            .send(HttpRequest::get(format!(
                "/v1/addresses/{}",
                self.payee_address
            )))
            .await?;
        let env: Envelope<AddressData> = parse_json(&resp.body)?;
        let acct = env.data.account_id;
        *self.payee_account.lock().unwrap_or_else(|e| e.into_inner()) = Some(acct.clone());
        Ok(acct)
    }
}

#[async_trait]
impl PaymentVerifier for MintVerifier {
    async fn verify(&self, auth: &PaymentAuthorization) -> Result<VerifiedHold, TurnstileError> {
        // Forward the agent's already-signed authorization verbatim — no re-sign.
        let body = serde_json::to_vec(auth).map_err(codec_err)?;
        let resp = self
            .send(HttpRequest::post_json("/v1/aitp/verify", body))
            .await?;
        let env: Envelope<VerifyData> = parse_json(&resp.body)?;

        let is_hold = env.data.scheme == "hold" || env.data.hold_id.is_some();
        let scheme = if is_hold {
            Scheme::Hold
        } else {
            Scheme::Transfer
        };
        Ok(VerifiedHold {
            hold: env.data.hold_id.map(HoldRef),
            scheme,
        })
    }

    async fn capture(
        &self,
        hold: &HoldRef,
        actual: AmountMicros,
    ) -> Result<Receipt, TurnstileError> {
        let to = self.payee_account_id().await?;
        let idem_key = format!("aitp:{}:capture", hold.0);
        let body = serde_json::to_vec(&CaptureReq {
            amount_micros: actual.get(),
            to: &to,
            idem_key: &idem_key,
        })
        .map_err(codec_err)?;

        let resp = self
            .send(HttpRequest::post_json(
                format!("/v1/holds/{}/capture", hold.0),
                body,
            ))
            .await?;
        let encoded = resp.header(HEADER_RECEIPT).ok_or_else(|| {
            TurnstileError::Pay(PayError::Codec(
                "capture response missing AITP-Receipt header".to_string(),
            ))
        })?;
        decode_receipt(encoded).map_err(TurnstileError::Pay)
    }

    async fn release(&self, hold: &HoldRef) -> Result<(), TurnstileError> {
        let idem_key = format!("aitp:{}:release", hold.0);
        let body = serde_json::to_vec(&ReleaseReq {
            idem_key: &idem_key,
        })
        .map_err(codec_err)?;
        self.send(HttpRequest::post_json(
            format!("/v1/holds/{}/release", hold.0),
            body,
        ))
        .await?;
        Ok(())
    }
}

/// A pure-unit [`PaymentVerifier`] that never touches the mint: it returns a
/// canned hold and records capture/release calls for assertions.
pub struct FakeVerifier {
    hold_id: Option<String>,
    calls: Mutex<FakeCalls>,
}

/// Recorded interactions with a [`FakeVerifier`].
#[derive(Debug, Default, Clone)]
pub struct FakeCalls {
    pub verified: usize,
    /// `(hold_id, actual_micros)` per capture.
    pub captured: Vec<(String, i64)>,
    /// `hold_id` per release.
    pub released: Vec<String>,
}

impl FakeVerifier {
    /// A verifier that verifies to an `aitp-hold` yielding `hold_id`.
    #[must_use]
    pub fn with_hold(hold_id: impl Into<String>) -> Self {
        Self {
            hold_id: Some(hold_id.into()),
            calls: Mutex::new(FakeCalls::default()),
        }
    }

    /// A verifier that verifies to an already-final `aitp-transfer` (no hold).
    #[must_use]
    pub fn transfer() -> Self {
        Self {
            hold_id: None,
            calls: Mutex::new(FakeCalls::default()),
        }
    }

    /// Snapshot the recorded calls.
    #[must_use]
    pub fn snapshot(&self) -> FakeCalls {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl PaymentVerifier for FakeVerifier {
    async fn verify(&self, _auth: &PaymentAuthorization) -> Result<VerifiedHold, TurnstileError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .verified += 1;
        let hold = self.hold_id.clone().map(HoldRef);
        let scheme = if hold.is_some() {
            Scheme::Hold
        } else {
            Scheme::Transfer
        };
        Ok(VerifiedHold { hold, scheme })
    }

    async fn capture(
        &self,
        hold: &HoldRef,
        actual: AmountMicros,
    ) -> Result<Receipt, TurnstileError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .captured
            .push((hold.0.clone(), actual.get()));
        Ok(Receipt {
            seq: 1,
            op: "hold.capture".to_string(),
            op_hash: String::new(),
            amount_micros: actual,
            balance_after_micros: AmountMicros::ZERO,
            prev_hash: String::new(),
            hash: String::new(),
            sig: String::new(),
        })
    }

    async fn release(&self, hold: &HoldRef) -> Result<(), TurnstileError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .released
            .push(hold.0.clone());
        Ok(())
    }
}

// ---- mint wire request/response shapes (mint-server §4) --------------------
//
// These mirror the shapes `aitp_client::mint` uses internally (they are private
// there). Turnstile reuses the aitp-client WIRE TYPES for the handshake but
// owns these mint-server envelope shapes for the verify/capture/release routes.

/// The mint success envelope `{ success:true, data }` — `success` is ignored.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: T,
}

/// `POST /v1/aitp/verify` → `data` for a settled authorization.
#[derive(Debug, Default, Deserialize)]
struct VerifyData {
    #[serde(default)]
    scheme: String,
    #[serde(default)]
    hold_id: Option<String>,
}

/// `GET /v1/addresses/:address` → `data`.
#[derive(Debug, Deserialize)]
struct AddressData {
    account_id: String,
}

/// `POST /v1/holds/:id/capture` request body.
#[derive(Debug, Serialize)]
struct CaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
}

/// `POST /v1/holds/:id/release` request body.
#[derive(Debug, Serialize)]
struct ReleaseReq<'a> {
    idem_key: &'a str,
}

/// The mint error envelope `{ success:false, error:{ code, message } }`.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

fn codec_err(e: serde_json::Error) -> TurnstileError {
    TurnstileError::Pay(PayError::Codec(e.to_string()))
}

fn parse_json<T: DeserializeOwned>(body: &[u8]) -> Result<T, TurnstileError> {
    serde_json::from_slice(body).map_err(codec_err)
}

/// Map a non-2xx mint response onto a typed error by its stable code.
fn map_error_response(resp: &HttpResponse) -> TurnstileError {
    let env: ErrorEnvelope = match parse_json(&resp.body) {
        Ok(e) => e,
        Err(_) => {
            return TurnstileError::Pay(PayError::Gateway(format!(
                "mint returned status {} with non-JSON body",
                resp.status
            )))
        }
    };
    TurnstileError::Pay(map_error_code(
        &env.error.code,
        &env.error.message,
        resp.status,
    ))
}

fn map_error_code(code: &str, message: &str, status: u16) -> PayError {
    match code {
        "insufficient_funds" => PayError::InsufficientFunds,
        "allowance_exceeded" => PayError::AllowanceExceeded,
        "hold_not_found" => PayError::HoldNotFound,
        "capture_exceeds_hold" => PayError::CaptureExceedsHold,
        "nonce_replayed" => PayError::NonceReplayed,
        "authorization_expired" => PayError::AuthorizationExpired,
        "signature_invalid" => PayError::SignatureInvalid,
        "account_frozen" => PayError::AccountFrozen,
        "idempotency_conflict" => PayError::IdempotencyConflict,
        other => PayError::Gateway(format!(
            "mint error (status {status}, code '{other}'): {message}"
        )),
    }
}

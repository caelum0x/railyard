//! The REAL, Mint-backed [`PaymentGateway`]: `MintPaymentGateway`.
//!
//! This is the production implementor of the payment seam. It answers a decoded
//! `402` by signing an [`PaymentAuthorization`] and settling it against a live
//! mint-server over HTTP:
//!
//! - [`authorize`](MintPaymentGateway::authorize) → `POST /v1/aitp/verify`
//!   (flat JSON `Authorization` body; the mint runs `verify_and_settle` and, for
//!   an `aitp-hold` challenge, returns a `hold_id`);
//! - [`capture`](MintPaymentGateway::capture) → `POST /v1/holds/:id/capture`
//!   (settles the actual amount to the payee — success path);
//! - [`release`](MintPaymentGateway::release) → `POST /v1/holds/:id/release`
//!   (frees the reservation — the agent pays nothing, SPEC ADR-0002).
//!
//! The gateway is generic over an injected [`HttpTransport`] (so tests drive it
//! with a fake that asserts the exact requests) and an [`AuthSigner`] (so the
//! ed25519 key material stays out of this type). Every protected route carries
//! an `X-API-Key` header.
//!
//! ## Interface gaps bridged here (flagged, not hidden)
//!
//! 1. **`capture` has no payee param.** The trait's `capture(&HoldRef, actual)`
//!    signature never supplies the `to` account the mint requires. The gateway
//!    therefore remembers the challenge payee across the authorize→capture gap
//!    (keyed by `hold_id`) and resolves it to an `AccountId` via
//!    `GET /v1/addresses/:address` immediately before capturing.
//! 2. **`capture` needs durable settlement evidence.** Mint returns the exact
//!    signed, hash-linked receipt in `AITP-Receipt`; the gateway requires and
//!    decodes that header instead of attempting to reconstruct evidence from a
//!    balance view.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::amount::AmountMicros;
use crate::codec::{decode_receipt, HEADER_RECEIPT};
use crate::error::PayError;
use crate::gateway::{Authorized, PaymentGateway};
use crate::signer::{op_hash, op_hash_hex, AuthSigner, AuthorizationIntent};
use crate::transport::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::types::{HoldRef, PaymentAuthorization, PaymentChallenge, Receipt, Scheme};

/// Header carrying the per-request API key on every protected mint route.
const HEADER_API_KEY: &str = "x-api-key";

/// What the gateway must remember from `authorize` to later `capture`/`release`
/// a hold (the trait threads only a [`HoldRef`], not the payee or idem key).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HoldContext {
    /// The provider `ait1…` address to resolve into a `to` `AccountId`.
    payee: String,
    /// The authorization's `idempotency_key`, the base for deterministic
    /// capture/release idem keys so retries are safe.
    idem_base: String,
}

/// The production, Mint-backed [`PaymentGateway`].
pub struct MintPaymentGateway {
    transport: Arc<dyn HttpTransport>,
    signer: Arc<dyn AuthSigner>,
    /// Paying account id (`acct_…`), read from config/env at startup.
    payer: String,
    /// mint-server API key (`X-API-Key`), read from a secret manager at startup.
    api_key: String,
    /// Payee/idem context per outstanding `hold_id` (see gap #1).
    holds: Mutex<HashMap<String, HoldContext>>,
}

impl MintPaymentGateway {
    /// Assemble a gateway over an injected transport and signer.
    ///
    /// `payer` is the paying `acct_…` id; `api_key` is the mint-server API key.
    /// Neither is hardcoded — production wiring reads them from the environment /
    /// a secret manager and passes them here.
    pub fn new(
        payer: impl Into<String>,
        api_key: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        signer: Arc<dyn AuthSigner>,
    ) -> Self {
        Self {
            transport,
            signer,
            payer: payer.into(),
            api_key: api_key.into(),
            holds: Mutex::new(HashMap::new()),
        }
    }

    /// Convenience constructor wiring a real [`ReqwestTransport`] to `base_url`.
    pub fn with_reqwest(
        base_url: impl Into<String>,
        payer: impl Into<String>,
        api_key: impl Into<String>,
        signer: Arc<dyn AuthSigner>,
    ) -> Result<Self, PayError> {
        let transport = Arc::new(ReqwestTransport::new(base_url)?);
        Ok(Self::new(payer, api_key, transport, signer))
    }

    /// Send a request with the API key attached, mapping non-2xx responses to a
    /// typed [`PayError`] via the mint error envelope.
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        let req = req.with_header(HEADER_API_KEY, &self.api_key);
        let resp = self.transport.send(req).await?;
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(map_error_response(&resp))
        }
    }

    /// Sign an authorization answering `challenge` with a real op_hash + sig.
    fn build_authorization(&self, challenge: &PaymentChallenge) -> PaymentAuthorization {
        // Deterministic idempotency key from the single-use nonce, so a retried
        // authorize collapses to the same ledger op server-side.
        let idempotency_key = format!("aitp:{}", challenge.nonce);
        let intent = AuthorizationIntent {
            payer: &self.payer,
            payee: &challenge.payee,
            amount_micros: challenge.amount_micros,
            nonce: &challenge.nonce,
            resource: &challenge.resource,
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key: &idempotency_key,
        };
        let hash = op_hash(&intent);
        PaymentAuthorization {
            payer: self.payer.clone(),
            payee: challenge.payee.clone(),
            amount_micros: challenge.amount_micros,
            nonce: challenge.nonce.clone(),
            resource: challenge.resource.clone(),
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key,
            pubkey_id: self.signer.pubkey_id(),
            op_hash: op_hash_hex(&hash),
            sig: self.signer.sign_op_hash(&hash),
        }
    }

    /// Resolve a provider `ait1…` address into its `AccountId` (the `to` a hold
    /// capture requires) via `GET /v1/addresses/:address`.
    async fn resolve_payee_account(&self, address: &str) -> Result<String, PayError> {
        let req = HttpRequest::get(format!("/v1/addresses/{address}"));
        let resp = self.send(req).await?;
        let env: Envelope<AddressData> = parse_json(&resp.body)?;
        Ok(env.data.account_id)
    }

    fn context_for(&self, hold_id: &str) -> Option<HoldContext> {
        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(hold_id)
            .cloned()
    }
}

#[async_trait]
impl PaymentGateway for MintPaymentGateway {
    fn hold_context(&self, hold: &HoldRef) -> Option<serde_json::Value> {
        self.context_for(&hold.0).and_then(|c| serde_json::to_value(c).ok())
    }

    fn restore_hold_context(&self, hold: &HoldRef, context: &serde_json::Value) -> Result<(), PayError> {
        let ctx: HoldContext =
            serde_json::from_value(context.clone()).map_err(|e| PayError::Codec(format!("hold context: {e}")))?;
        self.holds.lock().unwrap_or_else(|e| e.into_inner()).insert(hold.0.clone(), ctx);
        Ok(())
    }

    async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError> {
        let authorization = self.build_authorization(challenge);

        let body =
            serde_json::to_vec(&authorization).map_err(|e| PayError::Codec(e.to_string()))?;
        let resp = self
            .send(HttpRequest::post_json("/v1/aitp/verify", body))
            .await?;

        let env: Envelope<VerifyData> = parse_json(&resp.body)?;

        // `aitp-transfer` settles immediately (no hold); `aitp-hold` yields a
        // hold_id we must remember for capture/release. Prefer the challenge's
        // own scheme, but honor a mismatching server `scheme` defensively.
        let is_hold = matches!(challenge.scheme, Scheme::Hold) || env.data.scheme == "hold";
        let hold = if is_hold {
            let hold_id = env.data.hold_id.ok_or_else(|| {
                PayError::Gateway("verify returned hold scheme without hold_id".to_string())
            })?;
            self.holds.lock().unwrap_or_else(|e| e.into_inner()).insert(
                hold_id.clone(),
                HoldContext {
                    payee: challenge.payee.clone(),
                    idem_base: authorization.idempotency_key.clone(),
                },
            );
            Some(HoldRef(hold_id))
        } else {
            None
        };

        Ok(Authorized {
            authorization,
            hold,
        })
    }

    /// Settle the hold for `actual` to the payee — the SUCCESS path. The result
    /// is Mint's exact signed, hash-linked settlement receipt.
    async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError> {
        let ctx = self.context_for(&hold.0).ok_or_else(|| {
            PayError::Gateway(format!(
                "no payee context for hold {} (capture requires a prior authorize)",
                hold.0
            ))
        })?;

        // Resolve the provider address to the AccountId the mint's `to` needs.
        let to = self.resolve_payee_account(&ctx.payee).await?;
        let idem_key = format!("{}:capture", ctx.idem_base);

        let body = serde_json::to_vec(&CaptureReq {
            amount_micros: actual.get(),
            to: &to,
            idem_key: &idem_key,
        })
        .map_err(|e| PayError::Codec(e.to_string()))?;

        let resp = self
            .send(HttpRequest::post_json(
                format!("/v1/holds/{}/capture", hold.0),
                body,
            ))
            .await?;
        let encoded_receipt = resp.header(HEADER_RECEIPT).ok_or_else(|| {
            PayError::Codec("capture response missing AITP-Receipt header".to_string())
        })?;
        let receipt = decode_receipt(encoded_receipt)?;

        // Capture succeeded; drop the remembered context.
        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hold.0);

        Ok(receipt)
    }

    /// Release the hold without settling — the FAILURE path (agent pays nothing).
    async fn release(&self, hold: &HoldRef) -> Result<(), PayError> {
        // Fall back to the hold id as the idem base if authorize wasn't observed
        // (e.g. process restart); the mint still frees the reservation.
        let idem_base = self
            .context_for(&hold.0)
            .map(|c| c.idem_base)
            .unwrap_or_else(|| hold.0.clone());
        let idem_key = format!("{idem_base}:release");

        let body = serde_json::to_vec(&ReleaseReq {
            idem_key: &idem_key,
        })
        .map_err(|e| PayError::Codec(e.to_string()))?;

        self.send(HttpRequest::post_json(
            format!("/v1/holds/{}/release", hold.0),
            body,
        ))
        .await?;

        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&hold.0);
        Ok(())
    }
}

// ---- wire request/response shapes (mint-server §4) -------------------------

/// The mint success envelope `{ success:true, data }` — `success` is ignored.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: T,
}

/// `POST /v1/aitp/verify` → `data` for a settled authorization.
#[derive(Debug, Deserialize)]
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

/// `POST /v1/holds/:id/capture` request body (`dto::HoldCaptureReq`).
#[derive(Debug, Serialize)]
struct CaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
}

/// `POST /v1/holds/:id/release` request body (`dto::HoldReleaseReq`).
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

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, PayError> {
    serde_json::from_slice(body).map_err(|e| PayError::Codec(e.to_string()))
}

/// Map a non-2xx mint response onto a typed [`PayError`] by its stable error
/// code (mint-core `MintError::code()`), falling back to [`PayError::Gateway`].
fn map_error_response(resp: &HttpResponse) -> PayError {
    let env: ErrorEnvelope = match parse_json(&resp.body) {
        Ok(e) => e,
        // A non-2xx without a decodable envelope is an opaque gateway failure.
        Err(_) => {
            return PayError::Gateway(format!(
                "mint returned status {} with non-JSON body",
                resp.status
            ))
        }
    };
    map_error_code(&env.error.code, &env.error.message, resp.status)
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

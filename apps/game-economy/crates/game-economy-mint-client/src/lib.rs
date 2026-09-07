//! # game-economy-mint-client — the REAL HTTP transport to a live mint-server.
//!
//! [`HttpMintTransport`] implements [`game_economy_core::MintTransport`] over
//! `reqwest`, so a game storefront settles real card-pack / cosmetic / battle-pass
//! purchases on the same Mint rail the rest of Railyard uses — not just against
//! the in-memory FakeMint. It is a drop-in for `FakeMint`: the store depends on
//! the trait, and either transport can be injected.
//!
//! ## Contract (mirrors `apps/mint/crates/mint-server`)
//!
//! Endpoints (all under the `x-api-key`-guarded router):
//! - `create_hold`  -> `POST /v1/holds`               `{ from, amount_micros, idem_key, ref? }`
//! - `capture_hold` -> `POST /v1/holds/:id/capture`   `{ amount_micros, to, idem_key, ref? }`
//! - `release_hold` -> `POST /v1/holds/:id/release`   `{ idem_key, ref? }`
//! - `transfer`     -> `POST /v1/transfers`           `{ from, to, amount_micros, idem_key, ref? }`
//!
//! Every amount is a bare `i64` microdollar — never a float. Auth is the
//! `x-api-key` header, sent from config/env (never a literal). Responses are the
//! `{ success:true, data }` / `{ success:false, error:{ code, message } }`
//! envelope; the `data` payload is projected onto the crate's typed receipts.
//!
//! ## Idempotency
//!
//! The `GameStore` already spreads ONE purchase `idem_key` across the triple with
//! `:hold` / `:capture` / `:release` suffixes before it reaches the transport, so
//! this client forwards `req.idem_key` **verbatim** (exactly as the FakeMint
//! consumes it) — it does NOT re-suffix. A same-key/different-body replay is a
//! `409` the ledger reports as [`GameError::IdempotencyConflict`].
//!
//! ## Error mapping (never a raw throw)
//!
//! Transport faults (DNS, connect, timeout, decode) and every non-2xx status are
//! folded into a typed [`GameError`]. The stable `error.code` is consulted first,
//! then the HTTP status as a fallback:
//!
//! | code / status                        | `GameError`                     |
//! |--------------------------------------|---------------------------------|
//! | `insufficient_funds` / `402`         | `InsufficientFunds`             |
//! | `hold_not_found` / `404`             | `HoldNotFound` (terminal hold)  |
//! | `idempotency_conflict` / `409`       | `IdempotencyConflict`           |
//! | `capture_exceeds_hold` / `422`       | `CaptureExceedsHold`            |
//! | `account_frozen` / `kyc_required` / `403` | `Mint(..)` (no store variant) |
//! | anything else                        | `Mint("mint <status> <code>: <msg>")` |

use std::time::Duration;

use async_trait::async_trait;
use mint_protocol::{decode_receipt, HEADER_RECEIPT};
use serde::Deserialize;

use game_economy_core::error::GameError;
use game_economy_core::transport::{
    Capture, CaptureReceipt, Hold, HoldReceipt, MintTransport, Release, ReleaseReceipt,
    SettlementProof,
};

/// The HTTP header carrying the shared Mint API key (matches mint-server's
/// `auth::API_KEY_HEADER`).
const API_KEY_HEADER: &str = "x-api-key";

/// Env var for the mint-server base URL (e.g. `https://mint.internal`).
pub const ENV_MINT_URL: &str = "RAILYARD_MINT_URL";
/// Env var for the shared `x-api-key` secret.
pub const ENV_MINT_API_KEY: &str = "MINT_API_KEY";
/// Env var for the per-request timeout in milliseconds (optional).
pub const ENV_MINT_TIMEOUT_MS: &str = "RAILYARD_MINT_TIMEOUT_MS";
/// Default per-request timeout when `RAILYARD_MINT_TIMEOUT_MS` is unset.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A configured client bound to one mint-server deployment. Cheap to clone
/// (the inner `reqwest::Client` is an `Arc`).
#[derive(Clone)]
pub struct HttpMintTransport {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

impl HttpMintTransport {
    /// Build a client from a base URL and the shared `x-api-key` secret, using the
    /// [`DEFAULT_TIMEOUT`]. Both values come from config/env at startup — never a
    /// literal in source.
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Result<Self, GameError> {
        Self::with_timeout(base, api_key, DEFAULT_TIMEOUT)
    }

    /// Build a client with an explicit per-request timeout.
    pub fn with_timeout(
        base: impl Into<String>,
        api_key: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, GameError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| GameError::Mint(format!("build http client: {e}")))?;
        Ok(HttpMintTransport {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        })
    }

    /// Build a client from the environment: [`ENV_MINT_URL`], [`ENV_MINT_API_KEY`],
    /// and the optional [`ENV_MINT_TIMEOUT_MS`]. A missing required var is a typed
    /// [`GameError::Mint`] — the client fails fast at startup, never at first use.
    pub fn from_env() -> Result<Self, GameError> {
        let base = std::env::var(ENV_MINT_URL)
            .map_err(|_| GameError::Mint(format!("{ENV_MINT_URL} not set")))?;
        let api_key = std::env::var(ENV_MINT_API_KEY)
            .map_err(|_| GameError::Mint(format!("{ENV_MINT_API_KEY} not set")))?;
        let timeout = match std::env::var(ENV_MINT_TIMEOUT_MS) {
            Ok(raw) => {
                let ms: u64 = raw
                    .parse()
                    .map_err(|_| GameError::Mint(format!("{ENV_MINT_TIMEOUT_MS} not a u64")))?;
                Duration::from_millis(ms)
            }
            Err(_) => DEFAULT_TIMEOUT,
        };
        Self::with_timeout(base, api_key, timeout)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// POST a JSON body and decode either the `data` payload or a typed error.
    async fn post_json<B, T>(&self, path: &str, body: &B) -> Result<T, GameError>
    where
        B: serde::Serialize,
        T: for<'de> Deserialize<'de>,
    {
        let resp = self
            .http
            .post(self.url(path))
            .header(API_KEY_HEADER, &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| GameError::Mint(format!("mint request to {path} failed: {e}")))?;
        decode(resp).await
    }

    /// A terminal Mint operation is successful only when its signed receipt is
    /// present and decodable. Returning body data without evidence would create
    /// an entitlement the caller cannot independently audit.
    async fn post_json_receipted<B, T>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(T, SettlementProof), GameError>
    where
        B: serde::Serialize,
        T: for<'de> Deserialize<'de>,
    {
        let resp = self
            .http
            .post(self.url(path))
            .header(API_KEY_HEADER, &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| GameError::Mint(format!("mint request to {path} failed: {e}")))?;
        let encoded = resp
            .headers()
            .get(HEADER_RECEIPT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let data = decode(resp).await?;
        let encoded = encoded
            .ok_or_else(|| GameError::Mint("Mint terminal response missing AITP-Receipt".into()))?;
        let receipt = decode_receipt(&encoded)
            .map_err(|error| GameError::Mint(format!("decode Mint receipt: {error}")))?;
        Ok((
            data,
            SettlementProof {
                sequence: receipt.seq,
                operation: receipt.op,
                operation_hash: receipt.op_hash,
                receipt_hash: receipt.hash,
                signature: receipt.sig,
            },
        ))
    }
}

/// Decode a mint-server response: on 2xx unwrap `{success,data}` (falling back to
/// a bare body); otherwise map `(status, error.code, error.message)` to a typed
/// [`GameError`]. Body/transport decode failures are themselves typed, never raw.
async fn decode<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T, GameError> {
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| GameError::Mint(format!("read mint response body: {e}")))?;

    if (200..300).contains(&status) {
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| GameError::Mint(format!("decode mint success envelope: {e}")))?;
        // Success envelope: {"success":true,"data":<T>}; tolerate an un-enveloped
        // body by falling back to the whole value.
        let data = value.get("data").cloned().unwrap_or(value);
        return serde_json::from_value(data)
            .map_err(|e| GameError::Mint(format!("decode mint data payload: {e}")));
    }

    let (code, message) = serde_json::from_str::<MintErrorBody>(&text)
        .map(|b| (b.error.code, b.error.message))
        .unwrap_or_else(|_| (String::new(), text.clone()));
    Err(map_error(status, &code, message))
}

/// Map a mint-server failure `(status, code, message)` onto a [`GameError`]. The
/// stable code is authoritative; the HTTP status is the fallback for a missing or
/// unrecognized code. Mirrors mint-core's status table (§7) so the storefront
/// speaks the ledger's error vocabulary.
fn map_error(status: u16, code: &str, message: String) -> GameError {
    match (status, code) {
        // Stable-code matches (authoritative regardless of status).
        (_, "insufficient_funds") => GameError::InsufficientFunds,
        (_, "hold_not_found") => GameError::HoldNotFound,
        (_, "idempotency_conflict") => GameError::IdempotencyConflict,
        (_, "capture_exceeds_hold") => GameError::CaptureExceedsHold,
        // 403 family — the storefront has no dedicated frozen/KYC variant, so
        // these surface as the catch-all `Mint` with the ledger's own code.
        (_, "account_frozen") => GameError::Mint("account frozen".to_string()),
        (_, "kyc_required") => GameError::Mint("kyc required".to_string()),
        // Status fallbacks when the body carried no (recognized) code.
        (402, _) => GameError::InsufficientFunds,
        (404, _) => GameError::HoldNotFound,
        (409, _) => GameError::IdempotencyConflict,
        (422, _) => GameError::CaptureExceedsHold,
        (403, _) => GameError::Mint("account frozen or kyc required".to_string()),
        _ => GameError::Mint(format!("mint {status} {code}: {message}")),
    }
}

// ---- error envelope --------------------------------------------------------

/// The `{ success:false, error:{ code, message } }` shape mint-server returns.
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

// ---- wire request bodies (mint-server dto.rs) ------------------------------

#[derive(serde::Serialize)]
struct HoldCreateReq<'a> {
    from: &'a str,
    amount_micros: i64,
    idem_key: &'a str,
}

#[derive(serde::Serialize)]
struct HoldCaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
}

#[derive(serde::Serialize)]
struct HoldReleaseReq<'a> {
    idem_key: &'a str,
}

#[derive(serde::Serialize)]
struct TransferReq<'a> {
    from: &'a str,
    to: &'a str,
    amount_micros: i64,
    idem_key: &'a str,
}

// ---- wire response payloads (mint-server views.rs `hold_receipt`) ----------

/// `create` response `data`. The shared `hold_receipt` view omits the reserved
/// amount, so `create_hold` echoes the requested amount back into the receipt.
#[derive(Debug, Default, Deserialize)]
struct HoldData {
    #[serde(default)]
    hold_id: String,
    #[serde(default)]
    idempotent_replay: bool,
}

/// `capture` response `data`.
#[derive(Debug, Default, Deserialize)]
struct CaptureData {
    #[serde(default)]
    captured_micros: i64,
    #[serde(default)]
    holder_balance: BalanceView,
    #[serde(default)]
    idempotent_replay: bool,
}

/// `release` response `data`. mint-server's `hold_receipt` view does NOT carry the
/// released amount (a released hold shows `captured_micros: 0` and no reserved
/// figure), so `released_micros` is best-effort: populated only if a future view
/// adds the field. The storefront's success-only flow ignores this figure.
#[derive(Debug, Default, Deserialize)]
struct ReleaseData {
    #[serde(default)]
    released_micros: i64,
    #[serde(default)]
    idempotent_replay: bool,
}

/// A projected `Balance` — only the spendable figure the storefront reports as the
/// payer's post-settle balance.
#[derive(Debug, Default, Deserialize)]
struct BalanceView {
    #[serde(default)]
    available_micros: i64,
}

/// `transfer` response `data` (`transfer_receipt` view).
#[derive(Debug, Default, Deserialize)]
struct TransferData {
    #[serde(default)]
    first_seq: u64,
    #[serde(default)]
    idempotent_replay: bool,
}

/// The receipt from a direct `POST /v1/transfers` settlement.
#[derive(Debug, Clone)]
pub struct TransferReceipt {
    /// Ledger sequence number of the settlement.
    pub settlement_seq: u64,
    /// `true` when this was an idempotent replay.
    pub idempotent_replay: bool,
}

impl HttpMintTransport {
    /// `POST /v1/transfers` — a direct account-to-account settlement (no hold).
    /// Not part of the fixed-price storefront's hot path, but provided for
    /// symmetry with the ledger's transfer route (e.g. refunds / payouts).
    pub async fn transfer(
        &self,
        from: &str,
        to: &str,
        amount_micros: i64,
        idem_key: &str,
    ) -> Result<TransferReceipt, GameError> {
        let data: TransferData = self
            .post_json(
                "/v1/transfers",
                &TransferReq {
                    from,
                    to,
                    amount_micros,
                    idem_key,
                },
            )
            .await?;
        Ok(TransferReceipt {
            settlement_seq: data.first_seq,
            idempotent_replay: data.idempotent_replay,
        })
    }
}

#[async_trait]
impl MintTransport for HttpMintTransport {
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, GameError> {
        let data: HoldData = self
            .post_json(
                "/v1/holds",
                &HoldCreateReq {
                    from: &req.account_id,
                    amount_micros: req.amount_micros,
                    idem_key: &req.idem_key,
                },
            )
            .await?;
        Ok(HoldReceipt {
            hold_id: data.hold_id,
            // The shared view omits the reserved amount; echo what we asked for.
            amount_micros: req.amount_micros,
            idempotent_replay: data.idempotent_replay,
        })
    }

    async fn capture_hold(&self, hold_id: &str, req: Capture) -> Result<CaptureReceipt, GameError> {
        let (data, proof): (CaptureData, SettlementProof) = self
            .post_json_receipted(
                &format!("/v1/holds/{hold_id}/capture"),
                &HoldCaptureReq {
                    amount_micros: req.amount_micros,
                    to: &req.to,
                    idem_key: &req.idem_key,
                },
            )
            .await?;
        if proof.operation != "hold.capture" {
            return Err(GameError::Mint(format!(
                "Mint capture returned {} receipt",
                proof.operation
            )));
        }
        Ok(CaptureReceipt {
            captured_micros: data.captured_micros,
            balance_after_micros: data.holder_balance.available_micros,
            settlement_seq: proof.sequence,
            receipt_id: proof.receipt_hash.clone(),
            proof,
            idempotent_replay: data.idempotent_replay,
        })
    }

    async fn release_hold(&self, hold_id: &str, req: Release) -> Result<ReleaseReceipt, GameError> {
        let (data, proof): (ReleaseData, SettlementProof) = self
            .post_json_receipted(
                &format!("/v1/holds/{hold_id}/release"),
                &HoldReleaseReq {
                    idem_key: &req.idem_key,
                },
            )
            .await?;
        if proof.operation != "hold.release" {
            return Err(GameError::Mint(format!(
                "Mint release returned {} receipt",
                proof.operation
            )));
        }
        Ok(ReleaseReceipt {
            released_micros: data.released_micros,
            idempotent_replay: data.idempotent_replay,
            proof,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_stable_codes_to_typed_errors() {
        assert_eq!(
            map_error(402, "insufficient_funds", String::new()),
            GameError::InsufficientFunds
        );
        assert_eq!(
            map_error(404, "hold_not_found", String::new()),
            GameError::HoldNotFound
        );
        assert_eq!(
            map_error(409, "idempotency_conflict", String::new()),
            GameError::IdempotencyConflict
        );
        assert_eq!(
            map_error(422, "capture_exceeds_hold", String::new()),
            GameError::CaptureExceedsHold
        );
    }

    #[test]
    fn maps_bare_status_when_code_missing() {
        assert_eq!(
            map_error(402, "", String::new()),
            GameError::InsufficientFunds
        );
        assert_eq!(map_error(404, "", String::new()), GameError::HoldNotFound);
        assert_eq!(
            map_error(409, "", String::new()),
            GameError::IdempotencyConflict
        );
        assert_eq!(
            map_error(422, "", String::new()),
            GameError::CaptureExceedsHold
        );
    }

    #[test]
    fn frozen_and_kyc_fall_back_to_mint() {
        assert!(matches!(
            map_error(403, "account_frozen", String::new()),
            GameError::Mint(_)
        ));
        assert!(matches!(
            map_error(403, "kyc_required", String::new()),
            GameError::Mint(_)
        ));
    }

    #[test]
    fn unmapped_status_is_a_typed_mint_error() {
        match map_error(500, "boom", "kaboom".to_string()) {
            GameError::Mint(m) => {
                assert!(m.contains("500"));
                assert!(m.contains("boom"));
                assert!(m.contains("kaboom"));
            }
            other => panic!("expected Mint(..), got {other:?}"),
        }
    }
}

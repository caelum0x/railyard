//! The REAL, Mint-backed [`BillingLedger`]: [`MintLedger`].
//!
//! This is the PRODUCTION billing backend. It maps relay's reserve ->
//! settle/release lifecycle onto mint-server's stateful hold API over an injected
//! [`HttpTransport`]:
//!
//! * [`reserve`](MintLedger::reserve)  -> `POST /v1/holds` (retain the `hold_id`);
//! * [`settle`](MintLedger::settle)    -> `POST /v1/holds/:id/capture` (captures the
//!   EXACT actual to the payee; mint auto-releases the remainder, so the net debit
//!   is the actual, never the estimate — success-only preserved);
//! * [`release`](MintLedger::release)  -> `POST /v1/holds/:id/release` (frees the
//!   whole reservation; costs zero);
//! * [`available_micros`](MintLedger::available_micros) -> `GET /v1/accounts/:id/balance`.
//!
//! Money is bare `i64` microdollars on both sides ([`Micros::get`] <-> Mint
//! `amount_micros`), so there is no float bridge.
//!
//! ## Design notes (documented, not hidden)
//!
//! * **hold_id retention.** `POST /v1/holds` returns a `hold_id` that capture and
//!   release require, but relay's [`Reservation`] carries no id. So the ledger
//!   keeps a `Mutex<HashMap<JobId, HoldId>>` populated at `reserve` and consumed
//!   at the terminal op. This map is process-local and lost on restart — which is
//!   fine because a hold lives entirely within one request (reserve -> capture or
//!   release happen in a single middleware pass, including the panic/stream Drop
//!   paths). The entry is removed only after a CONFIRMED terminal success, so a
//!   transient capture error still leaves the hold available for the middleware's
//!   fallback release (no dangling Mint hold).
//! * **idempotency.** Idem keys are anchored on the relay [`JobId`]:
//!   `relay:{job}` (create), `relay:{job}:capture`, `relay:{job}:release`. Mint
//!   treats identical idem keys as no-ops, so middleware retries never double-charge.
//! * **payer account.** relay's `AccountId` is the API-key public id, while mint's
//!   `from` is an opaque `acct_…`. If a single `payer` account is configured every
//!   flow bills from it; otherwise the relay account id is used verbatim as the
//!   mint `from` (deployments that mint one wallet per relay account carry the
//!   `acct_…` id as the public id).
//! * **payee account.** relay's settle has no payee, but mint's capture needs a
//!   `to`; the configured `payee` (treasury/provider `acct_…`) receives the debit.

pub mod fake;
pub mod transport;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backend::BillingLedger;
use crate::domain::{AccountId, JobId, ModelId};
use crate::error::BillingError;
use crate::ledger::Reservation;
use crate::money::Micros;
use crate::usage::Usage;

use transport::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, TransportError};

/// The per-request mint API-key header.
const HEADER_API_KEY: &str = "x-api-key";

/// A Mint-backed [`BillingLedger`] driving mint-server over an injected transport.
pub struct MintLedger {
    transport: Arc<dyn HttpTransport>,
    /// mint-server API key (`X-API-Key`), read from a secret manager at startup.
    api_key: String,
    /// The mint account (`acct_…`) that receives captured funds on settle.
    payee: String,
    /// Optional single mint payer (`acct_…`). When `None`, the relay account id is
    /// used verbatim as the mint `from` account.
    payer: Option<String>,
    /// job -> hold_id, populated at reserve and consumed at the terminal op.
    holds: Mutex<HashMap<JobId, String>>,
}

impl MintLedger {
    /// Assemble a ledger over an injected transport.
    ///
    /// `api_key` is the mint-server API key; `payee` is the `acct_…` capture
    /// destination; `payer` optionally forces every hold to bill from one account.
    /// None of these are hardcoded — production wiring reads them from the
    /// environment / a secret manager.
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        api_key: impl Into<String>,
        payee: impl Into<String>,
        payer: Option<String>,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            payee: payee.into(),
            payer,
            holds: Mutex::new(HashMap::new()),
        }
    }

    /// Convenience constructor wiring a real [`ReqwestTransport`] to `base_url`.
    pub fn with_reqwest(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        payee: impl Into<String>,
        payer: Option<String>,
    ) -> Result<Self, TransportError> {
        let transport = Arc::new(ReqwestTransport::new(base_url)?);
        Ok(Self::new(transport, api_key, payee, payer))
    }

    /// The mint `from` account for a relay account: the configured single payer,
    /// or the relay account id used verbatim.
    fn from_account(&self, account: &AccountId) -> String {
        self.payer
            .clone()
            .unwrap_or_else(|| account.as_str().to_string())
    }

    fn hold_id(&self, job: &JobId) -> Option<String> {
        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job)
            .cloned()
    }

    fn remember(&self, job: JobId, hold_id: String) {
        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job, hold_id);
    }

    fn forget(&self, job: &JobId) {
        self.holds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job);
    }

    /// Send a request with the API key attached, classifying a non-2xx response
    /// into a typed [`MintFault`] via the mint error envelope.
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, MintFault> {
        let req = req.with_header(HEADER_API_KEY, &self.api_key);
        let resp = self
            .transport
            .send(req)
            .await
            .map_err(|e| MintFault::Transport(e.0))?;
        if resp.is_success() {
            return Ok(resp);
        }
        match parse_error(&resp.body) {
            Some((code, _message)) if is_insufficient(&code) => MintFault::insufficient(),
            Some((code, message)) => Err(MintFault::Api {
                status: resp.status,
                code,
                message,
            }),
            None => Err(MintFault::Api {
                status: resp.status,
                code: String::new(),
                message: format!("non-JSON body (status {})", resp.status),
            }),
        }
    }
}

#[async_trait]
impl BillingLedger for MintLedger {
    async fn reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        if estimate.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative reserve estimate {estimate:?}"
            )));
        }
        let from = self.from_account(account_id);
        let idem = format!("relay:{job}");
        let body = to_body(&HoldCreateReq {
            from: &from,
            amount_micros: estimate.get(),
            idem_key: &idem,
        })?;

        let resp = match self.send(HttpRequest::post_json("/v1/holds", body)).await {
            Ok(resp) => resp,
            Err(MintFault::Insufficient) => {
                // Mint's 402 does not report the available balance; the value is
                // unused at the edge (the challenge is sized from the requested
                // estimate), so surface ZERO rather than making a second call.
                return Err(BillingError::InsufficientCredits {
                    available: Micros::ZERO,
                    requested: estimate,
                });
            }
            Err(other) => return Err(other.into()),
        };

        let env: Envelope<HoldCreated> = decode(&resp.body)?;
        self.remember(job, env.data.hold_id);
        Ok(Reservation {
            job_id: job,
            account_id: account_id.clone(),
            estimate_micros: estimate,
        })
    }

    async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError> {
        // The model only labels the debit on the in-memory ledger; mint captures
        // by hold id + amount, so it is not carried on the wire here.
        let _ = model;
        let job = reservation.job_id;
        let Some(hold_id) = self.hold_id(&job) else {
            // No live hold: already captured/released (first-terminal-wins) or the
            // hold predates this process. Idempotent no-op — never a second debit.
            return Ok(());
        };

        let actual = usage.cost_micros;
        if actual.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative settle cost {actual:?}"
            )));
        }

        let idem = format!("relay:{job}:capture");
        let body = to_body(&CaptureReq {
            amount_micros: actual.get(),
            to: &self.payee,
            idem_key: &idem,
        })?;

        // Capture the EXACT actual; mint auto-releases the remainder of the hold,
        // so the net debit is the actual and never the reserved estimate. The hold
        // is only forgotten AFTER a confirmed capture, so a transient failure keeps
        // it available for the middleware's fallback release.
        self.send(HttpRequest::post_json(
            format!("/v1/holds/{hold_id}/capture"),
            body,
        ))
        .await?;
        self.forget(&job);
        Ok(())
    }

    async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let job = reservation.job_id;
        let Some(hold_id) = self.hold_id(&job) else {
            return Ok(()); // already terminal — idempotent no-op.
        };
        let idem = format!("relay:{job}:release");
        let body = to_body(&ReleaseReq { idem_key: &idem })?;
        self.send(HttpRequest::post_json(
            format!("/v1/holds/{hold_id}/release"),
            body,
        ))
        .await?;
        self.forget(&job);
        Ok(())
    }

    async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError> {
        let from = self.from_account(account_id);
        let resp = self
            .send(HttpRequest::get(format!("/v1/accounts/{from}/balance")))
            .await?;
        let env: Envelope<BalanceData> = decode(&resp.body)?;
        Ok(Micros::new(env.data.available_micros))
    }
}

// ---- typed transport faults ------------------------------------------------

/// A classified non-transport mint failure.
enum MintFault {
    /// Mint refused a hold for lack of funds/allowance.
    Insufficient,
    /// Any other protocol error carrying the stable mint error code.
    Api {
        status: u16,
        code: String,
        message: String,
    },
    /// A transport-level failure (DNS/connection/TLS/timeout).
    Transport(String),
}

impl MintFault {
    fn insufficient<T>() -> Result<T, MintFault> {
        Err(MintFault::Insufficient)
    }
}

impl From<MintFault> for BillingError {
    fn from(f: MintFault) -> Self {
        match f {
            // Insufficient surfacing on a non-reserve op is a logic/replay anomaly.
            MintFault::Insufficient => {
                BillingError::Store("mint reported insufficient funds on a settle/release".into())
            }
            MintFault::Api { code, message, .. } if code == "capture_exceeds_hold" => {
                BillingError::InvalidAmount(format!("capture exceeds hold: {message}"))
            }
            MintFault::Api {
                status,
                code,
                message,
            } => BillingError::Store(format!(
                "mint error (status {status}, code '{code}'): {message}"
            )),
            MintFault::Transport(m) => BillingError::Store(format!("mint transport: {m}")),
        }
    }
}

fn is_insufficient(code: &str) -> bool {
    matches!(
        code,
        "insufficient_funds" | "allowance_exceeded" | "reserve_insufficient"
    )
}

// ---- wire request/response shapes (mint-server contract) -------------------

#[derive(Debug, Serialize)]
struct HoldCreateReq<'a> {
    from: &'a str,
    amount_micros: i64,
    idem_key: &'a str,
}

#[derive(Debug, Serialize)]
struct CaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
}

#[derive(Debug, Serialize)]
struct ReleaseReq<'a> {
    idem_key: &'a str,
}

/// The mint success envelope `{ success:true, data }` — `success` is ignored.
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: T,
}

/// `POST /v1/holds` -> `data`.
#[derive(Debug, Deserialize)]
struct HoldCreated {
    hold_id: String,
}

/// `GET /v1/accounts/:id/balance` -> `data`.
#[derive(Debug, Deserialize)]
struct BalanceData {
    #[serde(default)]
    available_micros: i64,
}

fn to_body<T: Serialize>(value: &T) -> Result<Vec<u8>, BillingError> {
    serde_json::to_vec(value)
        .map_err(|e| BillingError::Store(format!("serialize mint request: {e}")))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, BillingError> {
    serde_json::from_slice(bytes).map_err(|e| BillingError::Store(format!("decode mint response: {e}")))
}

fn parse_error(body: &[u8]) -> Option<(String, String)> {
    #[derive(Deserialize)]
    struct ErrEnvelope {
        error: ErrBody,
    }
    #[derive(Deserialize)]
    struct ErrBody {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    }
    serde_json::from_slice::<ErrEnvelope>(body)
        .ok()
        .map(|e| (e.error.code, e.error.message))
}

#[cfg(test)]
mod tests {
    use super::fake::FakeMint;
    use super::*;
    use serde_json::Value;

    const API_KEY: &str = "mint-secret";
    const PAYEE: &str = "acct_treasury";

    fn acct() -> AccountId {
        AccountId::new("acct_payer")
    }

    fn usage(cost: i64) -> Usage {
        Usage::new(100, 50, 150, Micros::new(cost))
    }

    fn model() -> ModelId {
        ModelId::namespace("openai/gpt-4o")
    }

    fn ledger(fake: Arc<FakeMint>) -> MintLedger {
        MintLedger::new(fake, API_KEY, PAYEE, None)
    }

    fn body_json(req: &HttpRequest) -> Value {
        serde_json::from_slice(&req.body).expect("request body is json")
    }

    #[tokio::test]
    async fn reserve_posts_a_hold_and_retains_the_hold_id() {
        let fake = Arc::new(FakeMint::funded(10_000_000).with_hold_id("hold_ABC"));
        let l = ledger(fake.clone());
        let job = JobId::new();

        let res = l
            .reserve(&acct(), Micros::new(3_000_000), job)
            .await
            .expect("reserve");
        assert_eq!(res.job_id, job);
        assert_eq!(res.estimate_micros, Micros::new(3_000_000));

        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(holds.len(), 1, "exactly one hold created");
        let body = body_json(&holds[0]);
        assert_eq!(body["from"], "acct_payer");
        assert_eq!(body["amount_micros"], 3_000_000);
        assert_eq!(body["idem_key"], format!("relay:{job}"));
        // API key is attached.
        assert!(holds[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == API_KEY));

        // hold_id retained for the terminal op.
        assert_eq!(l.hold_id(&job).as_deref(), Some("hold_ABC"));
    }

    #[tokio::test]
    async fn settle_captures_exact_actual_to_the_payee() {
        let fake = Arc::new(FakeMint::funded(10_000_000).with_hold_id("hold_XYZ"));
        let l = ledger(fake.clone());
        let job = JobId::new();
        let res = l
            .reserve(&acct(), Micros::new(3_000_000), job)
            .await
            .expect("reserve");

        l.settle(&res, usage(1_234_567), model())
            .await
            .expect("settle");

        let caps = fake.matching("POST", "/v1/holds/hold_XYZ/capture");
        assert_eq!(caps.len(), 1, "exactly one capture");
        let body = body_json(&caps[0]);
        // The EXACT actual is captured, not the 3_000_000 estimate.
        assert_eq!(body["amount_micros"], 1_234_567);
        assert_eq!(body["to"], PAYEE);
        assert_eq!(body["idem_key"], format!("relay:{job}:capture"));

        // Hold consumed; a second settle is an idempotent no-op (no 2nd capture).
        assert_eq!(l.hold_id(&job), None);
        l.settle(&res, usage(1_234_567), model())
            .await
            .expect("idempotent settle");
        assert_eq!(fake.matching("POST", "/v1/holds/hold_XYZ/capture").len(), 1);
    }

    #[tokio::test]
    async fn release_frees_the_hold_and_costs_nothing() {
        let fake = Arc::new(FakeMint::funded(10_000_000).with_hold_id("hold_REL"));
        let l = ledger(fake.clone());
        let job = JobId::new();
        let res = l
            .reserve(&acct(), Micros::new(3_000_000), job)
            .await
            .expect("reserve");

        l.release(&res).await.expect("release");

        let rels = fake.matching("POST", "/v1/holds/hold_REL/release");
        assert_eq!(rels.len(), 1);
        assert_eq!(body_json(&rels[0])["idem_key"], format!("relay:{job}:release"));
        // No capture ever happened -> costs zero.
        assert!(fake.matching("POST", "/v1/holds/hold_REL/capture").is_empty());

        // Idempotent: a second release is a no-op.
        l.release(&res).await.expect("idempotent release");
        assert_eq!(fake.matching("POST", "/v1/holds/hold_REL/release").len(), 1);
    }

    #[tokio::test]
    async fn reserve_maps_mint_402_to_insufficient_credits() {
        let fake = Arc::new(FakeMint::insufficient());
        let l = ledger(fake.clone());

        let err = l
            .reserve(&acct(), Micros::new(9_000_000), JobId::new())
            .await
            .expect_err("insufficient");
        assert!(matches!(err, BillingError::InsufficientCredits { .. }));
        // Attempted exactly one hold; nothing retained.
        assert_eq!(fake.matching("POST", "/v1/holds").len(), 1);
    }

    #[tokio::test]
    async fn reserve_rejects_negative_estimate_without_calling_mint() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = ledger(fake.clone());
        let err = l
            .reserve(&acct(), Micros::new(-1), JobId::new())
            .await
            .expect_err("negative");
        assert!(matches!(err, BillingError::InvalidAmount(_)));
        assert!(fake.seen().is_empty(), "no mint call for a bad estimate");
    }

    #[tokio::test]
    async fn available_micros_reads_the_balance_route() {
        let fake = Arc::new(FakeMint::funded(7_654_321));
        let l = ledger(fake.clone());
        let avail = l.available_micros(&acct()).await.expect("balance");
        assert_eq!(avail, Micros::new(7_654_321));
        assert_eq!(fake.matching("GET", "/v1/accounts/acct_payer/balance").len(), 1);
    }

    #[tokio::test]
    async fn configured_payer_overrides_the_relay_account() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = MintLedger::new(fake.clone(), API_KEY, PAYEE, Some("acct_single".to_string()));
        l.reserve(&acct(), Micros::new(1_000), JobId::new())
            .await
            .expect("reserve");
        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(body_json(&holds[0])["from"], "acct_single");
    }
}

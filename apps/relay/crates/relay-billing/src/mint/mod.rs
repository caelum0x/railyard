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
//! * **durable hold identity.** `POST /v1/holds` returns a `hold_id` carried on
//!   [`Reservation`]. The gateway commits that reservation to its Postgres outbox
//!   before provider I/O. `MintLedger` holds no process-local money state, so a
//!   restarted worker can replay the exact terminal command from the durable row.
//! * **idempotency.** Idem keys are anchored on both the payer principal and
//!   relay [`JobId`]: `relay:{account}:{job}` plus a terminal suffix. Mint
//!   treats identical idem keys as no-ops, middleware retries never
//!   double-charge, and equal request ids from different principals cannot
//!   collide.
//! * **payer account.** relay's `AccountId` is the API-key public id, while mint's
//!   `from` is an opaque `acct_…`. If a single `payer` account is configured every
//!   flow bills from it; otherwise the relay account id is used verbatim as the
//!   mint `from` (deployments that mint one wallet per relay account carry the
//!   `acct_…` id as the public id).
//! * **payee account.** The configured `payee` (treasury/provider `acct_…`) is
//!   copied into the reservation and outbox before inference, so restart recovery
//!   cannot silently redirect a capture after configuration changes.
//! * **proof.** Production pins Mint Ed25519 verification keys and accepts a
//!   terminal result only when its signed hold attestation binds the exact local
//!   hold, payer, payee, amount, reference, idempotency key and receipt hash.

pub mod fake;
pub mod transport;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backend::{
    AccountCredit, BillingLedger, TerminalSettlement, TopUpCheckout, TopUpCheckoutRequest,
};
use crate::domain::{AccountId, JobId, ModelId};
use crate::error::BillingError;
use crate::ledger::Reservation;
use crate::money::Micros;
use crate::usage::Usage;
use aitp_client::codec::{decode_receipt, HEADER_RECEIPT};
use aitp_client::{
    HoldSettlementAttestation, HoldSettlementCommand, HoldSettlementExpectation,
    HoldSettlementOperation, PaymentAuthorization, Receipt,
};
use ed25519_dalek::VerifyingKey;

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
    /// Operator-pinned Mint receipt keys. Production requires at least one;
    /// tests may use the legacy constructor with an unsigned fake.
    verification_keys: HashMap<String, VerifyingKey>,
    require_attestation: bool,
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
            verification_keys: HashMap::new(),
            require_attestation: false,
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

    /// Production constructor with operator-pinned Mint verification keys.
    pub fn with_reqwest_verified(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        payee: impl Into<String>,
        payer: Option<String>,
        keys: impl IntoIterator<Item = (String, [u8; 32])>,
    ) -> Result<Self, TransportError> {
        let mut ledger = Self::with_reqwest(base_url, api_key, payee, payer)?;
        for (key_id, bytes) in keys {
            let key = VerifyingKey::from_bytes(&bytes).map_err(|error| {
                TransportError(format!("invalid Mint verification key: {error}"))
            })?;
            ledger.verification_keys.insert(key_id, key);
        }
        if ledger.verification_keys.is_empty() {
            return Err(TransportError(
                "at least one pinned Mint verification key is required".into(),
            ));
        }
        ledger.require_attestation = true;
        Ok(ledger)
    }

    /// The mint `from` account for a relay account: the configured single payer,
    /// or the relay account id used verbatim.
    fn payer_account(&self, account: &AccountId) -> String {
        self.payer
            .clone()
            .unwrap_or_else(|| account.as_str().to_string())
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
    async fn health(&self) -> Result<(), BillingError> {
        // Exercise both Mint's durable store and the configured API key. The
        // public `/readyz` probe cannot detect a rotated/revoked credential.
        self.send(HttpRequest::get(format!(
            "/v1/accounts/{}/balance",
            self.payee
        )))
        .await?;
        Ok(())
    }

    fn resolve_payer(&self, principal: &AccountId) -> AccountId {
        AccountId::new(self.payer_account(principal))
    }

    fn settlement_payee(&self) -> Option<AccountId> {
        Some(AccountId::new(self.payee.clone()))
    }

    async fn validate_payee_address(&self, address: &str) -> Result<(), BillingError> {
        let response = self
            .send(HttpRequest::get(format!("/v1/addresses/{address}")))
            .await?;
        let envelope: Envelope<AddressLookup> = decode(&response.body)?;
        if envelope.data.account_id != self.payee {
            return Err(BillingError::Store(format!(
                "AITP payee address resolves to '{}', but captures target '{}'",
                envelope.data.account_id, self.payee
            )));
        }
        Ok(())
    }

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
        let payer = self.resolve_payer(account_id);
        let idem = format!("relay:{payer}:{job}");
        self.reserve_resolved(account_id, &payer, estimate, job, &idem)
            .await
    }

    async fn reserve_resolved(
        &self,
        owner_principal: &AccountId,
        payer: &AccountId,
        estimate: Micros,
        job: JobId,
        idem: &str,
    ) -> Result<Reservation, BillingError> {
        if estimate.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative reserve estimate {estimate:?}"
            )));
        }
        let from = payer.as_str().to_string();
        let body = to_body(&HoldCreateReq {
            from: &from,
            amount_micros: estimate.get(),
            idem_key: idem,
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
        Ok(Reservation {
            job_id: job,
            owner_principal: owner_principal.clone(),
            account_id: payer.clone(),
            estimate_micros: estimate,
            held_amount_micros: estimate,
            backend_reference: Some(env.data.hold_id),
            settlement_payee: Some(AccountId::new(self.payee.clone())),
            execution_lease: None,
        })
    }

    async fn reserve_authorized(
        &self,
        owner_principal: &AccountId,
        authorization: &PaymentAuthorization,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        let body = to_body(authorization)?;
        let resp = match self
            .send(HttpRequest::post_json("/v1/aitp/verify", body))
            .await
        {
            Ok(resp) => resp,
            Err(MintFault::Insufficient) => {
                return Err(BillingError::InsufficientCredits {
                    available: Micros::ZERO,
                    requested: estimate,
                });
            }
            Err(other) => return Err(other.into()),
        };
        let env: Envelope<AitpVerified> = decode(&resp.body)?;
        let hold_id = env.data.hold_id.ok_or_else(|| {
            BillingError::Store("Mint AITP verify returned no hold_id".to_string())
        })?;
        let payer = AccountId::new(authorization.payer.clone());
        Ok(Reservation {
            job_id: job,
            owner_principal: owner_principal.clone(),
            account_id: payer,
            estimate_micros: estimate,
            held_amount_micros: Micros::new(authorization.amount_micros.get()),
            backend_reference: Some(hold_id),
            settlement_payee: Some(AccountId::new(self.payee.clone())),
            execution_lease: None,
        })
    }

    async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError> {
        self.settle_with_receipt(reservation, usage, model)
            .await
            .map(|_| ())
    }

    async fn settle_with_receipt(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<Option<Receipt>, BillingError> {
        Ok(self
            .settle_with_evidence(reservation, usage, model)
            .await?
            .receipt)
    }

    async fn settle_with_evidence(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<TerminalSettlement, BillingError> {
        // The model only labels the debit on the in-memory ledger; mint captures
        // by hold id + amount, so it is not carried on the wire here.
        let _ = model;
        let job = reservation.job_id;
        let hold_id = reservation
            .backend_reference
            .as_deref()
            .ok_or(BillingError::UnknownReservation(reservation.job_id))?;

        let actual = usage.cost_micros;
        if actual.is_negative() {
            return Err(BillingError::InvalidAmount(format!(
                "negative settle cost {actual:?}"
            )));
        }

        let idem = format!("relay:{}:{job}:capture", reservation.account_id);
        let reference = settlement_reference(reservation);
        let payee = reservation
            .settlement_payee
            .as_ref()
            .ok_or_else(|| BillingError::Store("Mint reservation has no immutable payee".into()))?;
        let body = to_body(&CaptureReq {
            amount_micros: actual.get(),
            to: payee.as_str(),
            idem_key: &idem,
            reference: &reference,
        })?;

        // Capture the EXACT actual; mint auto-releases the remainder of the hold,
        // so the net debit is the actual and never the reserved estimate. The hold
        // is only forgotten AFTER a confirmed capture, so a transient failure keeps
        // it available for the middleware's fallback release.
        let response = self
            .send(HttpRequest::post_json(
                format!("/v1/holds/{hold_id}/capture"),
                body,
            ))
            .await?;
        self.decode_and_verify_terminal(
            &response,
            reservation,
            &idem,
            &reference,
            HoldSettlementOperation::Capture {
                payee_account: payee.as_str(),
                amount_micros: actual.get(),
            },
        )
    }

    async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        self.release_with_evidence(reservation).await.map(|_| ())
    }

    async fn release_with_evidence(
        &self,
        reservation: &Reservation,
    ) -> Result<TerminalSettlement, BillingError> {
        let job = reservation.job_id;
        let hold_id = reservation
            .backend_reference
            .as_deref()
            .ok_or(BillingError::UnknownReservation(reservation.job_id))?;
        let idem = format!("relay:{}:{job}:release", reservation.account_id);
        let reference = settlement_reference(reservation);
        let body = to_body(&ReleaseReq {
            idem_key: &idem,
            reference: &reference,
        })?;
        let response = self
            .send(HttpRequest::post_json(
                format!("/v1/holds/{hold_id}/release"),
                body,
            ))
            .await?;
        self.decode_and_verify_terminal(
            &response,
            reservation,
            &idem,
            &reference,
            HoldSettlementOperation::Release {
                released_micros: reservation.held_amount_micros.get(),
            },
        )
    }

    async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError> {
        let from = self.payer_account(account_id);
        let resp = self
            .send(HttpRequest::get(format!("/v1/accounts/{from}/balance")))
            .await?;
        let env: Envelope<BalanceData> = decode(&resp.body)?;
        Ok(Micros::new(env.data.available_micros))
    }

    /// `POST /v1/checkouts` crediting the key's payer account.
    async fn create_top_up_checkout(
        &self,
        account_id: &AccountId,
        request: &TopUpCheckoutRequest,
    ) -> Result<TopUpCheckout, BillingError> {
        if request.amount_micros <= 0 || request.amount_micros % 10_000 != 0 {
            return Err(BillingError::InvalidAmount(
                "top-up amount must be a positive whole number of USD cents".into(),
            ));
        }
        let account = self.payer_account(account_id);
        let body = serde_json::json!({
            "provider": request.provider,
            "account_id": account,
            "amount_micros": request.amount_micros,
            "success_url": request.success_url,
            "cancel_url": request.cancel_url,
        });
        let body = serde_json::to_vec(&body)
            .map_err(|e| BillingError::Store(format!("encode checkout request: {e}")))?;
        let resp = self
            .send(HttpRequest::post_json("/v1/checkouts", body))
            .await?;
        let env: Envelope<TopUpCheckout> = decode(&resp.body)?;
        Ok(env.data)
    }

    /// `POST /v1/transfers` from an operator treasury into the payer wallet.
    async fn credit_account(
        &self,
        from_account: &str,
        account_id: &AccountId,
        amount: Micros,
        idem_key: &str,
        reference: &str,
    ) -> Result<AccountCredit, BillingError> {
        if amount.get() <= 0 {
            return Err(BillingError::InvalidAmount(
                "credit amount must be positive".into(),
            ));
        }
        let to = self.payer_account(account_id);
        let body = serde_json::json!({
            "from": from_account,
            "to": to,
            "amount_micros": amount.get(),
            "idem_key": idem_key,
            "ref": reference,
        });
        let body = serde_json::to_vec(&body)
            .map_err(|e| BillingError::Store(format!("encode transfer request: {e}")))?;
        let resp = self
            .send(HttpRequest::post_json("/v1/transfers", body))
            .await
            .map_err(|fault| match fault {
                MintFault::Insufficient => BillingError::InsufficientCredits {
                    available: Micros::new(0),
                    requested: amount,
                },
                other => other.into(),
            })?;
        let env: Envelope<TransferData> = decode(&resp.body)?;
        Ok(AccountCredit {
            from_account: from_account.to_string(),
            to_account: to,
            amount_micros: amount.get(),
            idempotent_replay: env.data.idempotent_replay,
        })
    }
}

/// `POST /v1/transfers` -> `data` (the fields Relay needs).
#[derive(Debug, Deserialize)]
struct TransferData {
    #[serde(default)]
    idempotent_replay: bool,
}

impl MintLedger {
    fn decode_terminal(&self, response: &HttpResponse) -> Result<TerminalSettlement, BillingError> {
        let encoded = response.header(HEADER_RECEIPT).ok_or_else(|| {
            BillingError::Store("Mint terminal response missing AITP-Receipt header".to_string())
        })?;
        let receipt = decode_receipt(encoded)
            .map_err(|error| BillingError::Store(format!("decode Mint receipt: {error}")))?;
        let envelope: Envelope<HoldTerminalData> = decode(&response.body)?;
        Ok(TerminalSettlement {
            receipt: Some(receipt),
            attestation: envelope.data.attestation,
        })
    }

    fn decode_and_verify_terminal(
        &self,
        response: &HttpResponse,
        reservation: &Reservation,
        idem: &str,
        reference: &str,
        operation: HoldSettlementOperation<'_>,
    ) -> Result<TerminalSettlement, BillingError> {
        let decoded = self.decode_terminal(response)?;
        self.verify_terminal(reservation, idem, reference, operation, decoded)
    }

    fn verify_terminal(
        &self,
        reservation: &Reservation,
        idem: &str,
        reference: &str,
        operation: HoldSettlementOperation<'_>,
        decoded: TerminalSettlement,
    ) -> Result<TerminalSettlement, BillingError> {
        if !self.require_attestation {
            return Ok(decoded);
        }
        let receipt = decoded
            .receipt
            .as_ref()
            .ok_or_else(|| BillingError::Store("Mint terminal response missing receipt".into()))?;
        let attestation = decoded.attestation.as_ref().ok_or_else(|| {
            BillingError::Store("Mint terminal response missing signed attestation".into())
        })?;
        let key = self
            .verification_keys
            .get(&attestation.signer_key_id)
            .ok_or_else(|| BillingError::Store("Mint attestation signer is not pinned".into()))?;
        let hold_id = reservation
            .backend_reference
            .as_deref()
            .ok_or(BillingError::UnknownReservation(reservation.job_id))?;
        let expectation = HoldSettlementExpectation {
            command: HoldSettlementCommand {
                hold_id,
                payer_account: reservation.account_id.as_str(),
                operation,
                idempotency_key: idem,
                reference: Some(reference),
            },
            receipt_hash: &receipt.hash,
        };
        let claims = attestation
            .verify_expected(&attestation.signer_key_id, key, &expectation)
            .map_err(|error| {
                BillingError::Store(format!(
                    "Mint terminal attestation verification failed: {error}"
                ))
            })?;
        if receipt.seq != claims.sequence
            || receipt.op != claims.operation
            || receipt.op_hash != claims.operation_hash
            || receipt.amount_micros.get() != claims.amount_micros
            || receipt.hash != claims.receipt_hash
        {
            return Err(BillingError::Store(
                "Mint receipt header conflicts with its verified terminal attestation".into(),
            ));
        }
        Ok(decoded)
    }
}

fn settlement_reference(reservation: &Reservation) -> String {
    format!(
        "relay:{}:{}",
        reservation.owner_principal, reservation.job_id
    )
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

#[derive(Debug, Deserialize)]
struct AddressLookup {
    account_id: String,
}

#[derive(Debug, Serialize)]
struct CaptureReq<'a> {
    amount_micros: i64,
    to: &'a str,
    idem_key: &'a str,
    #[serde(rename = "ref")]
    reference: &'a str,
}

#[derive(Debug, Serialize)]
struct ReleaseReq<'a> {
    idem_key: &'a str,
    #[serde(rename = "ref")]
    reference: &'a str,
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

#[derive(Debug, Deserialize)]
struct AitpVerified {
    #[serde(default)]
    hold_id: Option<String>,
}

/// `GET /v1/accounts/:id/balance` -> `data`.
#[derive(Debug, Deserialize)]
struct BalanceData {
    #[serde(default)]
    available_micros: i64,
}

#[derive(Debug, Deserialize)]
struct HoldTerminalData {
    #[serde(default)]
    attestation: Option<HoldSettlementAttestation>,
}

fn to_body<T: Serialize>(value: &T) -> Result<Vec<u8>, BillingError> {
    serde_json::to_vec(value)
        .map_err(|e| BillingError::Store(format!("serialize mint request: {e}")))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, BillingError> {
    serde_json::from_slice(bytes)
        .map_err(|e| BillingError::Store(format!("decode mint response: {e}")))
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
        assert_eq!(body["idem_key"], format!("relay:{}:{job}", acct()));
        // API key is attached.
        assert!(holds[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == API_KEY));

        // hold_id travels with the durable reservation instead of process memory.
        assert_eq!(res.backend_reference.as_deref(), Some("hold_ABC"));
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
        assert_eq!(body["idem_key"], format!("relay:{}:{job}:capture", acct()));

        // A retry sends the exact same idempotency key. Mint returns its original
        // terminal result, so at-least-once HTTP remains one effective capture.
        l.settle(&res, usage(1_234_567), model())
            .await
            .expect("idempotent settle");
        let retries = fake.matching("POST", "/v1/holds/hold_XYZ/capture");
        assert_eq!(retries.len(), 2);
        assert_eq!(
            body_json(&retries[0])["idem_key"],
            body_json(&retries[1])["idem_key"]
        );
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
        assert_eq!(
            body_json(&rels[0])["idem_key"],
            format!("relay:{}:{job}:release", acct())
        );
        // No capture ever happened -> costs zero.
        assert!(fake
            .matching("POST", "/v1/holds/hold_REL/capture")
            .is_empty());

        // Idempotent: a second release repeats the exact command; Mint is the
        // durable first-terminal-wins authority.
        l.release(&res).await.expect("idempotent release");
        let retries = fake.matching("POST", "/v1/holds/hold_REL/release");
        assert_eq!(retries.len(), 2);
        assert_eq!(
            body_json(&retries[0])["idem_key"],
            body_json(&retries[1])["idem_key"]
        );
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
    async fn production_mode_rejects_terminal_response_without_attestation() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let mut l = ledger(fake);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]);
        l.verification_keys
            .insert("pinned-key".into(), signing.verifying_key());
        l.require_attestation = true;
        let reservation = l
            .reserve(&acct(), Micros::new(3_000_000), JobId::new())
            .await
            .expect("reserve");
        let error = l
            .settle_with_evidence(&reservation, usage(1_000), model())
            .await
            .expect_err("unsigned Mint result must fail closed");
        assert!(error.to_string().contains("signed attestation"));
    }

    #[tokio::test]
    async fn available_micros_reads_the_balance_route() {
        let fake = Arc::new(FakeMint::funded(7_654_321));
        let l = ledger(fake.clone());
        let avail = l.available_micros(&acct()).await.expect("balance");
        assert_eq!(avail, Micros::new(7_654_321));
        assert_eq!(
            fake.matching("GET", "/v1/accounts/acct_payer/balance")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn health_exercises_mint_readiness_without_mutating_money() {
        let fake = Arc::new(FakeMint::funded(7_654_321));
        let l = ledger(fake.clone());
        l.health().await.expect("Mint readiness");
        assert_eq!(
            fake.matching("GET", "/v1/accounts/acct_treasury/balance")
                .len(),
            1
        );
        assert_eq!(fake.seen().len(), 1);
    }

    #[tokio::test]
    async fn configured_payer_overrides_the_relay_account() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = MintLedger::new(
            fake.clone(),
            API_KEY,
            PAYEE,
            Some("acct_single".to_string()),
        );
        l.reserve(&acct(), Micros::new(1_000), JobId::new())
            .await
            .expect("reserve");
        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(body_json(&holds[0])["from"], "acct_single");
    }

    #[tokio::test]
    async fn resolved_reserve_replay_ignores_later_payer_configuration() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = MintLedger::new(
            fake.clone(),
            API_KEY,
            PAYEE,
            Some("acct_new_config".to_string()),
        );
        let persisted = AccountId::new("acct_persisted");
        let job = JobId::new();
        l.reserve_resolved(
            &AccountId::new("relay_owner"),
            &persisted,
            Micros::new(1_000),
            job,
            "persisted-reserve-idem",
        )
        .await
        .expect("replay exact reserve");
        let hold = &fake.matching("POST", "/v1/holds")[0];
        assert_eq!(body_json(hold)["from"], "acct_persisted");
        assert_eq!(body_json(hold)["idem_key"], "persisted-reserve-idem");
    }

    #[tokio::test]
    async fn advertised_payee_must_resolve_to_capture_account() {
        let good = Arc::new(FakeMint::funded(1));
        ledger(good)
            .validate_payee_address("ait1qprovider")
            .await
            .expect("matching address mapping");

        let bad = Arc::new(FakeMint::funded(1).with_address_account("acct_other"));
        assert!(ledger(bad)
            .validate_payee_address("ait1qprovider")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn authorized_reservation_persists_the_full_signed_hold_amount() {
        use aitp_client::{AmountMicros, Scheme};

        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = ledger(fake);
        let authorization = PaymentAuthorization {
            payer: "acct_sponsor".into(),
            payee: "ait1qprovider".into(),
            amount_micros: AmountMicros(75_000),
            nonce: "nonce".into(),
            resource: "POST /v1/chat/completions".into(),
            expiry: i64::MAX,
            scheme: Scheme::Hold,
            idempotency_key: "authorized-hold".into(),
            pubkey_id: "payer-key".into(),
            op_hash: "op-hash".into(),
            sig: "signature".into(),
        };
        let reservation = l
            .reserve_authorized(
                &AccountId::new("relay_owner"),
                &authorization,
                Micros::new(50_000),
                JobId::new(),
            )
            .await
            .expect("authorized reserve");
        assert_eq!(reservation.estimate_micros, Micros::new(50_000));
        assert_eq!(reservation.held_amount_micros, Micros::new(75_000));
    }

    #[tokio::test]
    async fn equal_job_ids_are_isolated_by_principal() {
        let fake = Arc::new(FakeMint::funded(10_000_000));
        let l = ledger(fake.clone());
        let job = JobId::new();
        let alice = AccountId::new("acct_alice");
        let bob = AccountId::new("acct_bob");

        let alice_reservation = l
            .reserve(&alice, Micros::new(2_000), job)
            .await
            .expect("alice reserve");
        let bob_reservation = l
            .reserve(&bob, Micros::new(2_000), job)
            .await
            .expect("bob reserve");

        assert!(alice_reservation.backend_reference.is_some());
        assert!(bob_reservation.backend_reference.is_some());
        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(
            body_json(&holds[0])["idem_key"],
            format!("relay:{alice}:{job}")
        );
        assert_eq!(
            body_json(&holds[1])["idem_key"],
            format!("relay:{bob}:{job}")
        );

        l.settle(&alice_reservation, usage(1_000), model())
            .await
            .expect("alice settle");

        l.settle(&bob_reservation, usage(1_000), model())
            .await
            .expect("bob settle");
        assert_eq!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture").len(),
            2
        );
    }
}

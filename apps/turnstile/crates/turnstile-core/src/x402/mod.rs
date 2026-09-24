//! x402 support: Turnstile speaks the open x402 HTTP-402 payment standard
//! (x402 Foundation, Coinbase CDP, Stripe machine payments) next to AITP-402.
//!
//! Wire flow for a paid route when x402 is enabled:
//!
//! 1. Unpaid request → `402` whose JSON body is an x402 [`PaymentRequiredBody`]
//!    (`accepts[]` = USDC `exact` on the configured network), while the
//!    `AITP-Challenge` header keeps AITP-402 clients working unchanged.
//! 2. Retry with `X-PAYMENT` → [`X402Gate::verify`]: decode, check scheme,
//!    network, payee and amount locally, claim the EIP-3009 nonce in the
//!    [`X402Journal`] (replay guard, before the handler runs), then ask the
//!    facilitator to `/verify`.
//! 3. The protected handler runs. On a `2xx` → [`X402Gate::settle`] asks the
//!    facilitator to `/settle` on-chain, journals the transaction, credits the
//!    USDC into the payee's Mint account (`usdc:<tx>` deposit, re-verified by
//!    Mint on-chain) and returns the settlement for `X-PAYMENT-RESPONSE`.
//!    On a non-`2xx` nothing is settled: the payer's signed authorization is
//!    never submitted, so the payer is not charged (success-only billing).
//!
//! Metering note: x402 `exact` moves a fixed amount, so an x402 call is
//! charged the quoted price in full on success. Metered capture below the
//! quote remains available on the AITP-402 / Mint-hold rail.

pub mod config;
pub mod facilitator;
pub mod journal;
#[cfg(feature = "postgres")]
pub mod pg;
pub mod wire;

use std::sync::Arc;
use std::time::Duration;

use aitp_client::AmountMicros;
use serde_json::json;

use crate::clock::now_ms;
use crate::error::TurnstileError;

pub use config::X402Config;
pub use facilitator::{Facilitator, HttpFacilitator, MintCredit, MintDepositPoster};
pub use journal::{
    InMemoryX402Journal, MintCreditStatus, X402Journal, X402Phase, X402Record, X402Summary,
};
#[cfg(feature = "postgres")]
pub use pg::PostgresX402Journal;
pub use wire::{
    PaymentPayload, PaymentRequiredBody, PaymentRequirements, SettleResponse, VerifyResponse,
    HEADER_PAYMENT, HEADER_PAYMENT_RESPONSE, SCHEME_EXACT, X402_VERSION,
};

/// MIME type advertised for JSON tool responses.
pub const DEFAULT_MIME: &str = "application/json";

/// Why an `X-PAYMENT` was refused. `code` follows the x402 error vocabulary
/// where one exists; `retryable` marks infrastructure (not payer) failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X402Rejection {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl X402Rejection {
    fn payer(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: false,
        }
    }

    fn infra(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: true,
        }
    }
}

/// A verified, journaled payment whose handler may now run.
#[derive(Debug, Clone)]
pub struct X402Admission {
    pub payload: PaymentPayload,
    pub requirements: PaymentRequirements,
    pub nonce: String,
    pub payer: String,
    pub amount_micros: i64,
}

/// The x402 rail: builds requirements, verifies and settles payments.
pub struct X402Gate {
    config: X402Config,
    facilitator: Arc<dyn Facilitator>,
    journal: Arc<dyn X402Journal>,
    mint: Option<Arc<MintDepositPoster>>,
}

impl X402Gate {
    pub fn new(
        config: X402Config,
        facilitator: Arc<dyn Facilitator>,
        journal: Arc<dyn X402Journal>,
        mint: Option<Arc<MintDepositPoster>>,
    ) -> Self {
        Self {
            config,
            facilitator,
            journal,
            mint,
        }
    }

    #[must_use]
    pub fn config(&self) -> &X402Config {
        &self.config
    }

    #[must_use]
    pub fn journal(&self) -> &Arc<dyn X402Journal> {
        &self.journal
    }

    #[must_use]
    pub fn mint(&self) -> Option<&Arc<MintDepositPoster>> {
        self.mint.as_ref()
    }

    /// The absolute resource URL for a request path.
    #[must_use]
    pub fn resource_url(&self, path: &str) -> String {
        match &self.config.public_url {
            Some(origin) => format!("{origin}{path}"),
            None => path.to_string(),
        }
    }

    /// The requirements for one paid resource. USDC has 6 decimals, the same
    /// scale as microdollars, so the atomic amount equals the micro price.
    /// Returns `None` for a free (zero-priced) resource: x402 cannot express it.
    #[must_use]
    pub fn requirements(
        &self,
        path: &str,
        description: &str,
        price: AmountMicros,
        mime_type: &str,
    ) -> Option<PaymentRequirements> {
        if price.get() <= 0 {
            return None;
        }
        Some(PaymentRequirements {
            scheme: SCHEME_EXACT.to_string(),
            network: self.config.network.clone(),
            max_amount_required: price.get().to_string(),
            resource: self.resource_url(path),
            description: description.to_string(),
            mime_type: mime_type.to_string(),
            pay_to: self.config.pay_to.clone(),
            max_timeout_seconds: self.config.max_timeout_seconds,
            asset: self.config.asset.clone(),
            output_schema: None,
            extra: Some(json!({
                "name": self.config.asset_name,
                "version": self.config.asset_version,
            })),
        })
    }

    /// The `402` body advertising `accepts`.
    #[must_use]
    pub fn required_body(
        accepts: Vec<PaymentRequirements>,
        error: impl Into<String>,
    ) -> PaymentRequiredBody {
        PaymentRequiredBody {
            x402_version: X402_VERSION,
            error: error.into(),
            accepts,
        }
    }

    /// Verify an `X-PAYMENT` header against `requirements` and claim its nonce.
    pub async fn verify(
        &self,
        header: &str,
        requirements: &PaymentRequirements,
    ) -> Result<X402Admission, X402Rejection> {
        let payload = wire::decode_payment_header(header)
            .map_err(|e| X402Rejection::payer("invalid_payload", e))?;
        if payload.x402_version != X402_VERSION {
            return Err(X402Rejection::payer(
                "invalid_x402_version",
                format!(
                    "x402Version {} is not supported (expected 1)",
                    payload.x402_version
                ),
            ));
        }
        if payload.scheme != requirements.scheme {
            return Err(X402Rejection::payer(
                "invalid_scheme",
                format!("scheme {:?} is not accepted", payload.scheme),
            ));
        }
        if payload.network != requirements.network {
            return Err(X402Rejection::payer(
                "invalid_network",
                format!(
                    "network {:?} is not accepted (expected {:?})",
                    payload.network, requirements.network
                ),
            ));
        }
        let auth = payload
            .exact_authorization()
            .map_err(|e| X402Rejection::payer("invalid_payload", e))?;
        if !wire::same_address(&auth.to, &requirements.pay_to) {
            return Err(X402Rejection::payer(
                "invalid_exact_evm_payload_recipient_mismatch",
                "authorization pays a different address",
            ));
        }
        let required: u128 = requirements
            .max_amount_required
            .parse()
            .map_err(|_| X402Rejection::infra("unexpected_verify_error", "bad required amount"))?;
        let value: u128 = auth.value.trim().parse().map_err(|_| {
            X402Rejection::payer("invalid_payload", "authorization value is not an integer")
        })?;
        if value < required {
            return Err(X402Rejection::payer(
                "invalid_exact_evm_payload_authorization_value",
                format!("authorization value {value} is below the price {required}"),
            ));
        }
        let now_secs = (now_ms() / 1000).max(0) as u128;
        let valid_before: u128 = auth.valid_before.trim().parse().unwrap_or(0);
        if valid_before <= now_secs {
            return Err(X402Rejection::payer(
                "invalid_exact_evm_payload_authorization_valid_before",
                "authorization has expired",
            ));
        }
        if auth.nonce.trim().is_empty() || auth.nonce.len() > 130 {
            return Err(X402Rejection::payer(
                "invalid_payload",
                "authorization nonce is malformed",
            ));
        }
        // Journal what the payer actually signed (an overpayment settles in
        // full on-chain), so Turnstile's books and Mint's credit match the chain.
        let amount_micros = i64::try_from(value).map_err(|_| {
            X402Rejection::payer("invalid_payload", "authorization value is out of range")
        })?;

        // Claim the nonce BEFORE any side effect: a replayed payload stops here
        // on every replica sharing the journal.
        let created = now_ms();
        let record = X402Record {
            nonce: auth.nonce.clone(),
            resource: requirements.resource.clone(),
            network: requirements.network.clone(),
            payer: auth.from.clone(),
            pay_to: requirements.pay_to.clone(),
            amount_micros,
            phase: X402Phase::Reserved,
            transaction: None,
            mint_status: self.initial_credit_status(),
            mint_attempts: 0,
            last_error: String::new(),
            created_at_ms: created,
            updated_at_ms: created,
        };
        match self.journal.reserve(record).await {
            Ok(()) => {}
            Err(TurnstileError::NonceReplayed(_)) => {
                return Err(X402Rejection::payer(
                    "invalid_exact_evm_payload_authorization_nonce_used",
                    "this payment authorization was already used",
                ))
            }
            Err(e) => return Err(X402Rejection::infra("journal_unavailable", e.to_string())),
        }

        match self.facilitator.verify(&payload, requirements).await {
            Ok(verdict) if verdict.is_valid => Ok(X402Admission {
                payer: verdict.payer.unwrap_or(auth.from),
                nonce: auth.nonce,
                payload,
                requirements: requirements.clone(),
                amount_micros,
            }),
            Ok(verdict) => {
                let reason = verdict
                    .invalid_reason
                    .unwrap_or_else(|| "invalid_payment".to_string());
                self.abandon_quietly(&auth.nonce, &format!("verify rejected: {reason}"))
                    .await;
                Err(X402Rejection::payer(
                    &reason,
                    "the facilitator rejected the payment",
                ))
            }
            Err(e) => {
                self.abandon_quietly(&auth.nonce, &format!("verify failed: {e}"))
                    .await;
                Err(X402Rejection::infra("facilitator_unavailable", e))
            }
        }
    }

    /// The handler failed: release the claim without settling (payer not charged).
    pub async fn abandon(&self, admission: &X402Admission, reason: &str) {
        self.abandon_quietly(&admission.nonce, reason).await;
    }

    /// Settle a verified payment on-chain and credit it into Mint.
    pub async fn settle(&self, admission: &X402Admission) -> Result<SettleResponse, X402Rejection> {
        let outcome = self
            .facilitator
            .settle(&admission.payload, &admission.requirements)
            .await;
        let settled = match outcome {
            Ok(response) if response.success && !response.transaction.is_empty() => response,
            Ok(response) => {
                let reason = response
                    .error_reason
                    .unwrap_or_else(|| "settlement_failed".to_string());
                self.abandon_quietly(&admission.nonce, &format!("settle rejected: {reason}"))
                    .await;
                return Err(X402Rejection::payer(
                    &reason,
                    "the facilitator could not settle the payment",
                ));
            }
            Err(e) => {
                if let Err(journal_error) = self
                    .journal
                    .mark_unknown(&admission.nonce, &format!("settle failed in transit: {e}"))
                    .await
                {
                    eprintln!(
                        "x402: journaling unknown settlement {} failed: {journal_error}",
                        admission.nonce
                    );
                }
                return Err(X402Rejection::infra("facilitator_unavailable", e));
            }
        };

        let payer = settled
            .payer
            .clone()
            .unwrap_or_else(|| admission.payer.clone());
        if let Err(e) = self
            .journal
            .settle(
                &admission.nonce,
                &settled.transaction,
                &payer,
                self.initial_credit_status(),
            )
            .await
        {
            // The money moved on-chain; the payer gets the resource. Surface
            // the journaling fault loudly for the operator.
            eprintln!(
                "x402: settled tx {} for nonce {} but journaling failed: {e}",
                settled.transaction, admission.nonce
            );
            return Ok(settled);
        }
        if let Some(mint) = &self.mint {
            let credit = mint
                .credit(&settled.transaction, admission.amount_micros)
                .await;
            // The inline attempt never gives up; the reaper owns the retry budget.
            apply_credit(self.journal.as_ref(), &admission.nonce, credit, 0, None).await;
        }
        Ok(settled)
    }

    fn initial_credit_status(&self) -> MintCreditStatus {
        if self.mint.is_some() {
            MintCreditStatus::Pending
        } else {
            MintCreditStatus::NotConfigured
        }
    }

    async fn abandon_quietly(&self, nonce: &str, reason: &str) {
        if let Err(e) = self.journal.abandon(nonce, reason).await {
            eprintln!("x402: abandoning {nonce} failed: {e}");
        }
    }
}

/// Journal one Mint credit attempt. `Retry` stays pending until the attempt
/// count reaches `max_attempts` (when given).
async fn apply_credit(
    journal: &dyn X402Journal,
    nonce: &str,
    credit: MintCredit,
    attempts_so_far: i32,
    max_attempts: Option<i32>,
) {
    let exhausted = max_attempts.is_some_and(|max| attempts_so_far + 1 >= max);
    let (status, error) = match credit {
        MintCredit::Credited => (MintCreditStatus::Credited, String::new()),
        MintCredit::Retry(e) if exhausted => (
            MintCreditStatus::Failed,
            format!("gave up after {} attempts: {e}", attempts_so_far + 1),
        ),
        MintCredit::Retry(e) => (MintCreditStatus::Pending, e),
        MintCredit::Failed(e) => (MintCreditStatus::Failed, e),
    };
    if let Err(e) = journal.record_credit(nonce, status, &error).await {
        eprintln!("x402: recording Mint credit for {nonce} failed: {e}");
    }
}

/// Retry pending Mint credits once. Returns how many rows were attempted.
pub async fn credit_pending_once(
    journal: &dyn X402Journal,
    mint: &MintDepositPoster,
    batch: i64,
    max_attempts: i32,
) -> Result<usize, TurnstileError> {
    let pending = journal.pending_credits(batch).await?;
    for row in &pending {
        let Some(tx) = row.transaction.as_deref() else {
            continue;
        };
        let credit = mint.credit(tx, row.amount_micros).await;
        apply_credit(
            journal,
            &row.nonce,
            credit,
            row.mint_attempts,
            Some(max_attempts),
        )
        .await;
    }
    Ok(pending.len())
}

/// Background worker retrying Mint credits for settled x402 payments (the
/// chain may not be final when the request completes).
pub fn spawn_x402_credit_reaper(
    gate: Arc<X402Gate>,
    every: Duration,
    max_attempts: i32,
) -> Option<tokio::task::JoinHandle<()>> {
    let mint = gate.mint()?.clone();
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) =
                credit_pending_once(gate.journal().as_ref(), &mint, 50, max_attempts).await
            {
                eprintln!("x402: Mint credit sweep failed: {e}");
            }
        }
    }))
}

#[cfg(test)]
mod tests;

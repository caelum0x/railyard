//! Durable record of every x402 payment Turnstile accepted.
//!
//! Each row is keyed by the EIP-3009 authorization nonce (unique per signed
//! payment), which doubles as the replay guard: a payload whose nonce is
//! already journaled is refused BEFORE the protected handler runs, so a
//! replayed `X-PAYMENT` can never buy a second call even while the first
//! settlement is still in flight.
//!
//! Lifecycle: `reserved` (verified, handler running) → `settled` (on-chain tx)
//! or `abandoned` (handler failed or settle failed, nothing charged). Settled
//! rows then carry a Mint credit status: `pending` → `credited` | `failed`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::clock::now_ms;
use crate::error::TurnstileError;

/// Where one x402 payment is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum X402Phase {
    Reserved,
    Settled,
    Abandoned,
    /// The facilitator `/settle` call failed in transit: the transfer may or
    /// may not have landed on-chain. Shown to the operator for reconciliation
    /// against the payer address; never auto-retried (the payer's signature
    /// may still be valid, and a blind retry could double-settle).
    Unknown,
}

impl X402Phase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Settled => "settled",
            Self::Abandoned => "abandoned",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, TurnstileError> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "settled" => Ok(Self::Settled),
            "abandoned" => Ok(Self::Abandoned),
            "unknown" => Ok(Self::Unknown),
            other => Err(TurnstileError::Storage(format!(
                "unknown x402 phase {other:?}"
            ))),
        }
    }
}

/// Mint credit status of a settled payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MintCreditStatus {
    /// No Mint account configured; journaled in Turnstile only.
    NotConfigured,
    Pending,
    Credited,
    Failed,
}

impl MintCreditStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Pending => "pending",
            Self::Credited => "credited",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, TurnstileError> {
        match value {
            "not_configured" => Ok(Self::NotConfigured),
            "pending" => Ok(Self::Pending),
            "credited" => Ok(Self::Credited),
            "failed" => Ok(Self::Failed),
            other => Err(TurnstileError::Storage(format!(
                "unknown mint status {other:?}"
            ))),
        }
    }
}

/// One x402 payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct X402Record {
    /// EIP-3009 authorization nonce (primary key, replay guard).
    pub nonce: String,
    pub resource: String,
    pub network: String,
    pub payer: String,
    pub pay_to: String,
    pub amount_micros: i64,
    pub phase: X402Phase,
    /// On-chain transaction hash once settled.
    pub transaction: Option<String>,
    pub mint_status: MintCreditStatus,
    pub mint_attempts: i32,
    pub last_error: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Revenue roll-up for the operator console.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct X402Summary {
    pub settled_count: i64,
    pub settled_micros: i64,
    pub credited_micros: i64,
    pub pending_credit_count: i64,
    pub failed_credit_count: i64,
    pub abandoned_count: i64,
    pub unknown_count: i64,
}

#[async_trait]
pub trait X402Journal: Send + Sync {
    /// Claim `record.nonce`. Fails with [`TurnstileError::NonceReplayed`] if the
    /// nonce was ever journaled before.
    async fn reserve(&self, record: X402Record) -> Result<(), TurnstileError>;
    /// Mark a reserved payment settled on-chain.
    async fn settle(
        &self,
        nonce: &str,
        transaction: &str,
        payer: &str,
        mint_status: MintCreditStatus,
    ) -> Result<(), TurnstileError>;
    /// Mark a reserved payment abandoned (nothing charged).
    async fn abandon(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError>;
    /// Mark a reserved payment whose settlement outcome is unknown.
    async fn mark_unknown(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError>;
    /// Record the outcome of one Mint credit attempt.
    async fn record_credit(
        &self,
        nonce: &str,
        status: MintCreditStatus,
        error: &str,
    ) -> Result<(), TurnstileError>;
    /// Settled rows whose Mint credit is still pending, oldest first.
    async fn pending_credits(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError>;
    /// Most recent rows first.
    async fn recent(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError>;
    async fn summary(&self) -> Result<X402Summary, TurnstileError>;
}

/// Process-local journal (single replica / tests).
#[derive(Default)]
pub struct InMemoryX402Journal {
    rows: Mutex<BTreeMap<String, X402Record>>,
}

impl InMemoryX402Journal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_row<T>(
        &self,
        nonce: &str,
        f: impl FnOnce(&mut X402Record) -> Result<T, TurnstileError>,
    ) -> Result<T, TurnstileError> {
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| TurnstileError::Storage("x402 journal lock poisoned".into()))?;
        let row = rows
            .get_mut(nonce)
            .ok_or_else(|| TurnstileError::Settlement(format!("unknown x402 payment {nonce}")))?;
        f(row)
    }

    fn all(&self) -> Result<Vec<X402Record>, TurnstileError> {
        self.rows
            .lock()
            .map(|rows| rows.values().cloned().collect())
            .map_err(|_| TurnstileError::Storage("x402 journal lock poisoned".into()))
    }
}

#[async_trait]
impl X402Journal for InMemoryX402Journal {
    async fn reserve(&self, record: X402Record) -> Result<(), TurnstileError> {
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| TurnstileError::Storage("x402 journal lock poisoned".into()))?;
        if rows.contains_key(&record.nonce) {
            return Err(TurnstileError::NonceReplayed(record.nonce));
        }
        rows.insert(record.nonce.clone(), record);
        Ok(())
    }

    async fn settle(
        &self,
        nonce: &str,
        transaction: &str,
        payer: &str,
        mint_status: MintCreditStatus,
    ) -> Result<(), TurnstileError> {
        self.with_row(nonce, |row| {
            if row.phase != X402Phase::Reserved {
                return Err(TurnstileError::Settlement(format!(
                    "x402 payment {nonce} is already {}",
                    row.phase.as_str()
                )));
            }
            row.phase = X402Phase::Settled;
            row.transaction = Some(transaction.to_string());
            if !payer.is_empty() {
                row.payer = payer.to_string();
            }
            row.mint_status = mint_status;
            row.updated_at_ms = now_ms();
            Ok(())
        })
    }

    async fn abandon(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError> {
        self.with_row(nonce, |row| {
            if row.phase == X402Phase::Reserved {
                row.phase = X402Phase::Abandoned;
                row.last_error = reason.to_string();
                row.updated_at_ms = now_ms();
            }
            Ok(())
        })
    }

    async fn mark_unknown(&self, nonce: &str, reason: &str) -> Result<(), TurnstileError> {
        self.with_row(nonce, |row| {
            if row.phase == X402Phase::Reserved {
                row.phase = X402Phase::Unknown;
                row.last_error = reason.to_string();
                row.updated_at_ms = now_ms();
            }
            Ok(())
        })
    }

    async fn record_credit(
        &self,
        nonce: &str,
        status: MintCreditStatus,
        error: &str,
    ) -> Result<(), TurnstileError> {
        self.with_row(nonce, |row| {
            row.mint_status = status;
            row.mint_attempts += 1;
            row.last_error = error.to_string();
            row.updated_at_ms = now_ms();
            Ok(())
        })
    }

    async fn pending_credits(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError> {
        let mut rows: Vec<X402Record> = self
            .all()?
            .into_iter()
            .filter(|r| r.phase == X402Phase::Settled && r.mint_status == MintCreditStatus::Pending)
            .collect();
        rows.sort_by_key(|r| r.updated_at_ms);
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }

    async fn recent(&self, limit: i64) -> Result<Vec<X402Record>, TurnstileError> {
        let mut rows = self.all()?;
        rows.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }

    async fn summary(&self) -> Result<X402Summary, TurnstileError> {
        Ok(summarize(&self.all()?))
    }
}

/// Roll a set of rows up into an [`X402Summary`].
#[must_use]
pub fn summarize(rows: &[X402Record]) -> X402Summary {
    let mut s = X402Summary::default();
    for row in rows {
        match row.phase {
            X402Phase::Settled => {
                s.settled_count += 1;
                s.settled_micros += row.amount_micros;
                match row.mint_status {
                    MintCreditStatus::Credited => s.credited_micros += row.amount_micros,
                    MintCreditStatus::Pending => s.pending_credit_count += 1,
                    MintCreditStatus::Failed => s.failed_credit_count += 1,
                    MintCreditStatus::NotConfigured => {}
                }
            }
            X402Phase::Abandoned => s.abandoned_count += 1,
            X402Phase::Unknown => s.unknown_count += 1,
            X402Phase::Reserved => {}
        }
    }
    s
}

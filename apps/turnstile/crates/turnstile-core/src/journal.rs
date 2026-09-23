//! Durable ownership and terminal-intent journal for Turnstile holds.
//!
//! Mint remains the money authority. This journal records enough immutable
//! command state for another Turnstile replica to replay verification,
//! capture, or release without running the protected handler again.

use std::collections::HashMap;
use std::sync::Mutex;

use aitp_client::{AmountMicros, HoldRef, PaymentAuthorization, Receipt, Scheme};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::TurnstileError;

/// Monotonic lifecycle of one consumed authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementPhase {
    Verifying,
    Held,
    CapturePending,
    Captured,
    ReleasePending,
    Released,
    TransferComplete,
    Rejected,
}

impl SettlementPhase {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Captured | Self::Released | Self::TransferComplete | Self::Rejected
        )
    }
}

/// Persisted command and recovery ownership for one authorization nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub authorization: PaymentAuthorization,
    pub phase: SettlementPhase,
    pub hold: Option<HoldRef>,
    pub scheme: Option<Scheme>,
    pub actual_micros: Option<AmountMicros>,
    pub receipt: Option<Receipt>,
    pub owner_token: String,
    pub lease_until_ms: i64,
    pub last_error: String,
}

impl SettlementRecord {
    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.authorization.nonce
    }
}

/// Storage contract used by request handlers and the recovery worker.
#[async_trait]
pub trait SettlementJournal: Send + Sync {
    async fn begin_verification(
        &self,
        authorization: &PaymentAuthorization,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError>;

    async fn bind_verified(
        &self,
        nonce: &str,
        owner_token: &str,
        hold: Option<&HoldRef>,
        scheme: Scheme,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError>;

    async fn renew(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<(), TurnstileError>;

    async fn prepare_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        actual: AmountMicros,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError>;

    async fn prepare_release(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError>;

    async fn complete_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        receipt: &Receipt,
    ) -> Result<(), TurnstileError>;

    async fn complete_release(&self, nonce: &str, owner_token: &str) -> Result<(), TurnstileError>;

    async fn reject_verification(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError>;

    async fn record_error(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError>;

    /// Atomically lease expired non-terminal commands to one recovery worker.
    async fn claim_recoverable(
        &self,
        worker_token: &str,
        now_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<SettlementRecord>, TurnstileError>;

    async fn get(&self, nonce: &str) -> Result<Option<SettlementRecord>, TurnstileError>;

    /// The most recent settlement commands, newest first, optionally filtered
    /// to one phase. Powers the operator console (paid calls, captured
    /// revenue, stuck captures). Journals that cannot enumerate return an
    /// explicit storage error rather than an empty list.
    async fn list_recent(
        &self,
        _limit: usize,
        _phase: Option<SettlementPhase>,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        Err(TurnstileError::Storage(
            "this settlement journal does not support listing".into(),
        ))
    }
}

/// Single-process journal used by tests and explicit single-replica deployments.
#[derive(Default)]
pub struct InMemorySettlementJournal {
    records: Mutex<HashMap<String, SettlementRecord>>,
}

impl InMemorySettlementJournal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_record<R>(
        &self,
        nonce: &str,
        owner_token: &str,
        update: impl FnOnce(&mut SettlementRecord) -> Result<R, TurnstileError>,
    ) -> Result<R, TurnstileError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let record = records
            .get_mut(nonce)
            .ok_or_else(|| TurnstileError::Settlement("settlement record not found".into()))?;
        if record.owner_token != owner_token {
            return Err(TurnstileError::Settlement(
                "settlement execution lease is not owned by this worker".into(),
            ));
        }
        update(record)
    }
}

#[async_trait]
impl SettlementJournal for InMemorySettlementJournal {
    async fn begin_verification(
        &self,
        authorization: &PaymentAuthorization,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = records.get(&authorization.nonce) {
            if existing.authorization == *authorization {
                return Ok(existing.clone());
            }
            return Err(TurnstileError::Settlement(
                "authorization nonce is bound to another settlement command".into(),
            ));
        }
        let record = SettlementRecord {
            authorization: authorization.clone(),
            phase: SettlementPhase::Verifying,
            hold: None,
            scheme: None,
            actual_micros: None,
            receipt: None,
            owner_token: owner_token.to_string(),
            lease_until_ms,
            last_error: String::new(),
        };
        records.insert(authorization.nonce.clone(), record.clone());
        Ok(record)
    }

    async fn bind_verified(
        &self,
        nonce: &str,
        owner_token: &str,
        hold: Option<&HoldRef>,
        scheme: Scheme,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.authorization.scheme != scheme {
                return Err(TurnstileError::Settlement(
                    "Mint verification scheme differs from authorized scheme".into(),
                ));
            }
            if record.phase != SettlementPhase::Verifying {
                return Err(TurnstileError::Settlement(
                    "verified hold can only bind from verifying state".into(),
                ));
            }
            if scheme == Scheme::Hold && hold.is_none() {
                return Err(TurnstileError::Settlement(
                    "hold verification returned no hold identity".into(),
                ));
            }
            if scheme == Scheme::Transfer && hold.is_some() {
                return Err(TurnstileError::Settlement(
                    "transfer verification unexpectedly returned a hold identity".into(),
                ));
            }
            record.hold = hold.cloned();
            record.scheme = Some(scheme);
            record.phase = if scheme == Scheme::Hold {
                SettlementPhase::Held
            } else {
                SettlementPhase::TransferComplete
            };
            record.lease_until_ms = lease_until_ms;
            record.last_error.clear();
            Ok(record.clone())
        })
    }

    async fn renew(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<(), TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if !matches!(
                record.phase,
                SettlementPhase::Verifying | SettlementPhase::Held
            ) {
                return Err(TurnstileError::Settlement(
                    "only active request ownership can be renewed".into(),
                ));
            }
            record.lease_until_ms = lease_until_ms;
            Ok(())
        })
    }

    async fn prepare_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        actual: AmountMicros,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if actual <= AmountMicros::ZERO || actual > record.authorization.amount_micros {
                return Err(TurnstileError::Settlement(
                    "capture amount is outside the authorized quote".into(),
                ));
            }
            if record.phase == SettlementPhase::CapturePending
                && record.actual_micros == Some(actual)
            {
                return Ok(record.clone());
            }
            if record.phase != SettlementPhase::Held {
                return Err(TurnstileError::Settlement(
                    "capture intent conflicts with settlement state".into(),
                ));
            }
            record.phase = SettlementPhase::CapturePending;
            record.actual_micros = Some(actual);
            record.lease_until_ms = lease_until_ms;
            record.last_error.clear();
            Ok(record.clone())
        })
    }

    async fn prepare_release(
        &self,
        nonce: &str,
        owner_token: &str,
        lease_until_ms: i64,
    ) -> Result<SettlementRecord, TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.phase == SettlementPhase::ReleasePending {
                return Ok(record.clone());
            }
            if record.phase != SettlementPhase::Held {
                return Err(TurnstileError::Settlement(
                    "release intent conflicts with settlement state".into(),
                ));
            }
            record.phase = SettlementPhase::ReleasePending;
            record.actual_micros = None;
            record.lease_until_ms = lease_until_ms;
            record.last_error.clear();
            Ok(record.clone())
        })
    }

    async fn complete_capture(
        &self,
        nonce: &str,
        owner_token: &str,
        receipt: &Receipt,
    ) -> Result<(), TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.phase != SettlementPhase::CapturePending
                || record.actual_micros != Some(receipt.amount_micros)
            {
                return Err(TurnstileError::Settlement(
                    "capture receipt does not match pending intent".into(),
                ));
            }
            record.phase = SettlementPhase::Captured;
            record.receipt = Some(receipt.clone());
            record.last_error.clear();
            Ok(())
        })
    }

    async fn complete_release(&self, nonce: &str, owner_token: &str) -> Result<(), TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.phase != SettlementPhase::ReleasePending {
                return Err(TurnstileError::Settlement(
                    "release completion has no pending intent".into(),
                ));
            }
            record.phase = SettlementPhase::Released;
            record.last_error.clear();
            Ok(())
        })
    }

    async fn reject_verification(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.phase != SettlementPhase::Verifying {
                return Err(TurnstileError::Settlement(
                    "verification rejection conflicts with settlement state".into(),
                ));
            }
            record.phase = SettlementPhase::Rejected;
            record.last_error = error.to_string();
            Ok(())
        })
    }

    async fn record_error(
        &self,
        nonce: &str,
        owner_token: &str,
        error: &str,
    ) -> Result<(), TurnstileError> {
        self.with_record(nonce, owner_token, |record| {
            if record.phase.is_terminal() {
                return Err(TurnstileError::Settlement(
                    "terminal settlement cannot record a pending error".into(),
                ));
            }
            record.last_error = error.to_string();
            Ok(())
        })
    }

    async fn claim_recoverable(
        &self,
        worker_token: &str,
        now_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut nonces = records
            .values()
            .filter(|record| !record.phase.is_terminal() && record.lease_until_ms <= now_ms)
            .map(|record| record.nonce().to_string())
            .collect::<Vec<_>>();
        nonces.sort();
        let recoverable = nonces
            .into_iter()
            .take(limit)
            .map(|nonce| {
                let record = records
                    .get_mut(&nonce)
                    .expect("selected settlement record must remain present");
                record.owner_token = worker_token.to_string();
                record.lease_until_ms = lease_until_ms;
                record.clone()
            })
            .collect::<Vec<_>>();
        Ok(recoverable)
    }

    async fn get(&self, nonce: &str) -> Result<Option<SettlementRecord>, TurnstileError> {
        Ok(self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(nonce)
            .cloned())
    }

    async fn list_recent(
        &self,
        limit: usize,
        phase: Option<SettlementPhase>,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        let records = self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut out: Vec<SettlementRecord> = records
            .values()
            .filter(|record| phase.is_none_or(|p| record.phase == p))
            .cloned()
            .collect();
        // Challenge expiry is issue time + a fixed TTL, so it orders by recency.
        out.sort_by(|a, b| b.authorization.expiry.cmp(&a.authorization.expiry));
        out.truncate(limit);
        Ok(out)
    }
}

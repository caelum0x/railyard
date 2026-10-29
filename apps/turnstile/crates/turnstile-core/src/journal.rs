//! Durable ownership and terminal-intent journal for Turnstile holds.
//!
//! Mint remains the money authority. This journal records enough immutable
//! command state for another Turnstile replica to replay verification,
//! capture, or release without running the protected handler again.

use std::collections::{BTreeMap, HashMap};
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

/// Captured revenue for one gated resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceRevenue {
    pub resource: String,
    pub paid_calls: u64,
    pub captured_micros: i64,
}

/// Lifetime revenue and settlement health across the whole journal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RevenueSummary {
    /// Settlement commands counted.
    pub total_calls: u64,
    /// Captured (or transfer-complete) revenue in µ$.
    pub captured_micros: i64,
    /// Authorized but not yet captured (held / capture pending / verifying).
    pub pending_authorized_micros: i64,
    /// Command count per phase tag.
    pub by_phase: BTreeMap<String, u64>,
    /// Captured revenue per resource, ordered by resource.
    pub by_resource: Vec<ResourceRevenue>,
}

impl RevenueSummary {
    /// Aggregate a set of records (used by journals without native aggregates).
    #[must_use]
    pub fn from_records(records: &[SettlementRecord]) -> Self {
        let mut summary = Self {
            total_calls: records.len() as u64,
            ..Self::default()
        };
        let mut by_resource: BTreeMap<String, (u64, i64)> = BTreeMap::new();
        for record in records {
            *summary
                .by_phase
                .entry(record.phase.tag().to_string())
                .or_default() += 1;
            match record.phase {
                SettlementPhase::Captured | SettlementPhase::TransferComplete => {
                    let amount = record
                        .actual_micros
                        .map_or(record.authorization.amount_micros.0, |a| a.0);
                    summary.captured_micros = summary.captured_micros.saturating_add(amount);
                    let entry = by_resource
                        .entry(record.authorization.resource.clone())
                        .or_default();
                    entry.0 += 1;
                    entry.1 = entry.1.saturating_add(amount);
                }
                SettlementPhase::Held
                | SettlementPhase::CapturePending
                | SettlementPhase::Verifying => {
                    summary.pending_authorized_micros = summary
                        .pending_authorized_micros
                        .saturating_add(record.authorization.amount_micros.0);
                }
                SettlementPhase::ReleasePending
                | SettlementPhase::Released
                | SettlementPhase::Rejected => {}
            }
        }
        summary.by_resource = by_resource
            .into_iter()
            .map(|(resource, (paid_calls, captured_micros))| ResourceRevenue {
                resource,
                paid_calls,
                captured_micros,
            })
            .collect();
        summary
    }
}

impl SettlementPhase {
    /// The stable snake_case tag (same as the serde / database form).
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Verifying => "verifying",
            Self::Held => "held",
            Self::CapturePending => "capture_pending",
            Self::Captured => "captured",
            Self::ReleasePending => "release_pending",
            Self::Released => "released",
            Self::TransferComplete => "transfer_complete",
            Self::Rejected => "rejected",
        }
    }
}

/// Constant-time (with respect to WHERE the two byte slices differ) equality.
/// Always walks the full length of the longer input instead of returning as
/// soon as a mismatch is found.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether `presented` is a byte-for-byte match of `stored`, compared in
/// constant time with respect to where the two differ.
///
/// Both `begin_verification` implementations ([`InMemorySettlementJournal`]
/// and [`crate::pg_settlement::PostgresSettlementJournal`]) use this to decide
/// whether a resubmitted nonce is an idempotent retry of the same
/// [`PaymentAuthorization`] (including its `sig` field, which only the real
/// payer's private key could have produced) or a different command
/// colliding on the same nonce. Derived `PartialEq`/`==` on this struct is a
/// thin wrapper over `memcmp` and returns as soon as it finds the first
/// differing byte — the same non-constant-time leak found and fixed
/// 2026-10-29 in `operator::settlement_for_payer`'s payer-proof check (see
/// that commit for a measured timing delta). Comparing the full canonical
/// JSON encoding in one constant-time pass closes the same class of leak
/// here, on the hot path every payment admission goes through.
pub(crate) fn authorization_matches(
    presented: &PaymentAuthorization,
    stored: &PaymentAuthorization,
) -> bool {
    match (serde_json::to_vec(presented), serde_json::to_vec(stored)) {
        (Ok(a), Ok(b)) => constant_time_eq(&a, &b),
        _ => false,
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

    /// One page of settlement commands, newest first, skipping `offset` rows.
    /// Durable journals override this with a native `LIMIT/OFFSET`.
    async fn list_page(
        &self,
        limit: usize,
        offset: usize,
        phase: Option<SettlementPhase>,
    ) -> Result<Vec<SettlementRecord>, TurnstileError> {
        let rows = self.list_recent(limit.saturating_add(offset), phase).await?;
        Ok(rows.into_iter().skip(offset).take(limit).collect())
    }

    /// Lifetime revenue and health across every recorded command (not just a
    /// recent window), so revenue from retired tools stays visible.
    async fn revenue_summary(&self) -> Result<RevenueSummary, TurnstileError> {
        let rows = self.list_recent(usize::MAX, None).await?;
        Ok(RevenueSummary::from_records(&rows))
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
            if authorization_matches(authorization, &existing.authorization) {
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

#[cfg(test)]
mod tests {
    //! Regression tests for the bug found 2026-10-29: `begin_verification`'s
    //! idempotent-retry check (both here and in
    //! `pg_settlement::PostgresSettlementJournal`) used derived `PartialEq`
    //! (`existing.authorization == *authorization` / `!=`) — equivalent to
    //! plain `==` on every field including `sig`, the one field only the real
    //! payer's private key could have produced. This is the exact same
    //! non-constant-time comparison pattern found and fixed the same day in
    //! `operator::settlement_for_payer`'s payer-proof check, just hit on the
    //! payment-admission hot path instead of the settlement-status lookup.
    //! This test only pins the functional behavior (idempotent retries still
    //! accepted; a near-miss anywhere, including just `sig`, is still
    //! rejected) — the timing property itself was verified out-of-band
    //! against the same `constant_time_eq` primitive `operator.rs` already
    //! validates.
    use super::*;

    fn auth(sig: &str) -> PaymentAuthorization {
        PaymentAuthorization {
            payer: "acct_payer".into(),
            payee: "ait1qpayee".into(),
            amount_micros: AmountMicros(1_000),
            nonce: "nonce123".into(),
            resource: "POST /v1/echo".into(),
            expiry: 1_000_000,
            scheme: Scheme::Hold,
            idempotency_key: "idem_1".into(),
            pubkey_id: "key_1".into(),
            op_hash: "hash1".into(),
            sig: sig.into(),
        }
    }

    #[test]
    fn identical_authorizations_match() {
        assert!(authorization_matches(&auth("realsig"), &auth("realsig")));
    }

    #[test]
    fn a_mismatch_in_only_the_last_byte_of_sig_is_still_rejected() {
        assert!(!authorization_matches(&auth("realsiH"), &auth("realsig")));
    }

    #[test]
    fn a_mismatch_in_the_first_byte_of_sig_is_rejected() {
        assert!(!authorization_matches(&auth("Xealsig"), &auth("realsig")));
    }

    #[tokio::test]
    async fn begin_verification_is_idempotent_on_an_exact_retry() {
        let journal = InMemorySettlementJournal::new();
        let a = auth("realsig");
        journal
            .begin_verification(&a, "worker-1", 1_000)
            .await
            .unwrap();
        // Same nonce, byte-for-byte identical authorization resubmitted (the
        // ordinary retry-after-timeout case) must still succeed.
        let again = journal.begin_verification(&a, "worker-1", 2_000).await;
        assert!(again.is_ok());
    }

    #[tokio::test]
    async fn begin_verification_rejects_a_same_nonce_sig_near_miss() {
        let journal = InMemorySettlementJournal::new();
        journal
            .begin_verification(&auth("realsig"), "worker-1", 1_000)
            .await
            .unwrap();
        // Same nonce, every field identical except the last byte of sig —
        // the exact near-miss a timing attacker would probe for.
        let collision = journal
            .begin_verification(&auth("realsiH"), "worker-1", 1_000)
            .await;
        assert!(collision.is_err());
    }
}

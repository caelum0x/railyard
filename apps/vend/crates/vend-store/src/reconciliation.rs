//! Durable leasing and atomic local convergence for the reconciliation worker.
//!
//! Mint is authoritative for money. These methods only claim bounded work and
//! update Vend's projection after the caller has observed Mint's hold state.

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::Row;

use vend_core::{ChargeId, VendError};

use crate::{store_err, PendingUnattachedHold, VendStore};

const LEASE_UNATTACHED_SQL: &str = "WITH candidates AS (\
 SELECT u.charge_id, a.mint_account_id FROM metered_usage u \
 JOIN apps a ON a.id = u.app_id \
 WHERE u.outcome = 'pending' AND u.hold_id = '' AND u.created_at < $1 \
 AND (u.reconcile_lease_until IS NULL OR u.reconcile_lease_until < now()) \
 ORDER BY u.created_at, u.charge_id LIMIT $2 FOR UPDATE OF u SKIP LOCKED\
) UPDATE metered_usage u SET reconcile_lease_token = $3, \
 reconcile_lease_until = $4, reconcile_attempts = reconcile_attempts + 1, \
 reconcile_last_error = NULL FROM candidates c WHERE u.charge_id = c.charge_id \
 RETURNING u.charge_id, u.idem_key, u.charge_micros, c.mint_account_id";

const LEASE_ATTACHED_SQL: &str = "WITH candidates AS (\
 SELECT u.charge_id, (SELECT EXISTS (SELECT 1 FROM payout_executions p \
  WHERE p.charge_id = u.charge_id AND (p.provider_transferred OR \
  p.tenant_transferred OR p.platform_transferred))) AS payout_started \
 FROM metered_usage u WHERE u.outcome = 'pending' AND u.hold_id <> '' AND u.created_at < $1 \
 AND (u.reconcile_lease_until IS NULL OR u.reconcile_lease_until < now()) \
 ORDER BY u.created_at, u.charge_id LIMIT $2 FOR UPDATE OF u SKIP LOCKED\
) UPDATE metered_usage u SET reconcile_lease_token = $3, \
 reconcile_lease_until = $4, reconcile_attempts = reconcile_attempts + 1, \
 reconcile_last_error = NULL FROM candidates c WHERE u.charge_id = c.charge_id \
 RETURNING u.charge_id, u.idem_key, u.hold_id, c.payout_started";

const LEASE_PAYOUT_SQL: &str = "WITH candidates AS (\
 SELECT p.charge_id FROM payout_executions p \
 JOIN metered_usage u ON u.charge_id = p.charge_id \
 WHERE p.state = 'pending' AND u.outcome = 'captured' AND p.updated_at < $1 \
 AND (p.reconcile_lease_until IS NULL OR p.reconcile_lease_until < now()) \
 ORDER BY p.updated_at, p.charge_id LIMIT $2 FOR UPDATE OF p SKIP LOCKED\
) UPDATE payout_executions p SET reconcile_lease_token = $3, \
 reconcile_lease_until = $4, reconcile_attempts = reconcile_attempts + 1 \
 FROM candidates c WHERE p.charge_id = c.charge_id \
 RETURNING p.charge_id, \
 (SELECT hold_id FROM metered_usage WHERE charge_id = p.charge_id) AS hold_id";

#[derive(Clone, Debug)]
pub struct AttachedHoldCandidate {
    pub charge_id: ChargeId,
    pub idem_key: String,
    pub hold_id: String,
    /// Fail-closed signal: an open/released hold may only cancel a payout plan
    /// when no transfer leg was ever observed.
    pub payout_started: bool,
}

#[derive(Clone, Debug)]
pub struct PayoutCandidate {
    pub charge_id: ChargeId,
    pub hold_id: String,
}

fn validate_claim(limit: i64, lease_seconds: i64, worker_token: &str) -> Result<(), VendError> {
    if !(1..=1_000).contains(&limit)
        || !(30..=3_600).contains(&lease_seconds)
        || worker_token.trim().is_empty()
        || worker_token.len() > 200
    {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

/// Stable terminal record used when Mint captured but the original upstream
/// response was lost. Its schema is deliberately the same `ReplayRecord` read
/// by Vend's non-streaming data plane; streaming treats any completed record as
/// consumed without inspecting the payload.
pub fn captured_recovery_replay(
    request_hash: &str,
    charge_id: &ChargeId,
    provider_micros: i64,
    price_micros: i64,
) -> String {
    json!({
        "request_hash": request_hash,
        "outcome": {
            "status": 409,
            "body": {"error": {
                "message": "The request was charged before Vend restarted; its upstream response cannot be replayed",
                "type": "vend_error", "param": null, "code": "settled_response_unavailable"
            }},
            "charge_id": charge_id.as_str(),
            "base_cost_micros": provider_micros,
            "price_micros": price_micros,
            "model": null
        }
    })
    .to_string()
}

impl VendStore {
    /// Lease pre-attachment crash rows. Repeating Mint hold creation and release
    /// is still idempotent, while the lease prevents avoidable duplicate calls.
    pub async fn lease_unattached_holds(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
        lease_seconds: i64,
        worker_token: &str,
    ) -> Result<Vec<PendingUnattachedHold>, VendError> {
        validate_claim(limit, lease_seconds, worker_token)?;
        let lease_until = Utc::now() + Duration::seconds(lease_seconds);
        let rows = sqlx::query(LEASE_UNATTACHED_SQL)
            .bind(older_than)
            .bind(limit)
            .bind(worker_token)
            .bind(lease_until)
            .fetch_all(self.pool())
            .await
            .map_err(store_err)?;
        rows.into_iter()
            .map(|row| {
                Ok(PendingUnattachedHold {
                    charge_id: ChargeId::parse(
                        &row.try_get::<String, _>("charge_id").map_err(store_err)?,
                    )?,
                    idem_key: row.try_get("idem_key").map_err(store_err)?,
                    payer_account: row.try_get("mint_account_id").map_err(store_err)?,
                    amount_micros: row.try_get("charge_micros").map_err(store_err)?,
                })
            })
            .collect()
    }

    pub async fn finish_leased_unattached_release(
        &self,
        charge_id: &ChargeId,
        worker_token: &str,
    ) -> Result<(), VendError> {
        let mut tx = self.pool().begin().await.map_err(store_err)?;
        let row = sqlx::query(
            "UPDATE metered_usage SET outcome = 'released', reconcile_lease_token = NULL, \
               reconcile_lease_until = NULL, reconcile_last_error = NULL \
             WHERE charge_id = $1 AND outcome = 'pending' AND hold_id = '' \
               AND reconcile_lease_token = $2 RETURNING idem_key",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or(VendError::IdempotencyConflict)?;
        let idem_key: String = row.try_get("idem_key").map_err(store_err)?;
        let replay_key = idem_key
            .strip_prefix("vend:")
            .ok_or_else(|| VendError::Storage("invalid Vend idempotency prefix".into()))?;
        sqlx::query("DELETE FROM idempotency_keys WHERE key = $1 AND state = 'pending'")
            .bind(replay_key)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        tx.commit().await.map_err(store_err)
    }

    /// Atomically lease stale pending usage rows which already reference a Mint
    /// hold. `SKIP LOCKED` makes concurrent workers select disjoint batches.
    pub async fn lease_attached_holds(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
        lease_seconds: i64,
        worker_token: &str,
    ) -> Result<Vec<AttachedHoldCandidate>, VendError> {
        validate_claim(limit, lease_seconds, worker_token)?;
        let lease_until = Utc::now() + Duration::seconds(lease_seconds);
        let rows = sqlx::query(LEASE_ATTACHED_SQL)
            .bind(older_than)
            .bind(limit)
            .bind(worker_token)
            .bind(lease_until)
            .fetch_all(self.pool())
            .await
            .map_err(store_err)?;
        rows.into_iter()
            .map(|row| {
                Ok(AttachedHoldCandidate {
                    charge_id: ChargeId::parse(
                        &row.try_get::<String, _>("charge_id").map_err(store_err)?,
                    )?,
                    idem_key: row.try_get("idem_key").map_err(store_err)?,
                    hold_id: row.try_get("hold_id").map_err(store_err)?,
                    payout_started: row.try_get("payout_started").map_err(store_err)?,
                })
            })
            .collect()
    }

    /// Converge a Mint-open-or-released hold to a local release and reopen the
    /// exact client key for retry. Any staged payout is cancelled atomically;
    /// observed payout legs make release convergence fail closed.
    pub async fn finish_attached_release(
        &self,
        charge_id: &ChargeId,
        worker_token: &str,
    ) -> Result<(), VendError> {
        let mut tx = self.pool().begin().await.map_err(store_err)?;
        let unsafe_plan = sqlx::query(
            "SELECT 1 FROM payout_executions WHERE charge_id = $1 \
             AND (provider_transferred OR tenant_transferred OR platform_transferred)",
        )
        .bind(charge_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .is_some();
        if unsafe_plan {
            return Err(VendError::Storage(
                "cannot release a charge with observed payout transfers".into(),
            ));
        }
        let row = sqlx::query(
            "UPDATE metered_usage SET outcome = 'released', reconcile_lease_token = NULL, \
               reconcile_lease_until = NULL, reconcile_last_error = NULL \
             WHERE charge_id = $1 AND outcome = 'pending' AND reconcile_lease_token = $2 \
             RETURNING idem_key",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or(VendError::IdempotencyConflict)?;
        let idem_key: String = row.try_get("idem_key").map_err(store_err)?;
        let replay_key = idem_key
            .strip_prefix("vend:")
            .ok_or_else(|| VendError::Storage("invalid Vend idempotency prefix".into()))?;
        sqlx::query(
            "UPDATE payout_executions SET state = 'cancelled', last_error = NULL, \
               reconcile_lease_token = NULL, reconcile_lease_until = NULL, updated_at = now() \
             WHERE charge_id = $1 AND state = 'pending'",
        )
        .bind(charge_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;
        sqlx::query("DELETE FROM idempotency_keys WHERE key = $1 AND state = 'pending'")
            .bind(replay_key)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        tx.commit().await.map_err(store_err)
    }

    /// Restore the exact rated usage from the immutable pre-capture payout plan
    /// and consume a stranded client claim in the same transaction. The replay
    /// is intentionally a deterministic recovery response: inference bytes are
    /// not retained, so Vend must never pretend it can replay them.
    pub async fn finish_attached_capture(
        &self,
        charge_id: &ChargeId,
        worker_token: &str,
    ) -> Result<(), VendError> {
        let mut tx = self.pool().begin().await.map_err(store_err)?;
        let row = sqlx::query(
            "SELECT u.idem_key, p.provider_cost_micros, p.captured_charge_micros, \
               p.platform_fee_micros, p.tenant_net_micros \
             FROM metered_usage u JOIN payout_executions p ON p.charge_id = u.charge_id \
             WHERE u.charge_id = $1 AND u.outcome = 'pending' \
               AND u.reconcile_lease_token = $2 AND p.state = 'pending'",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            VendError::Storage("captured hold has no pending immutable payout plan".into())
        })?;
        let idem_key: String = row.try_get("idem_key").map_err(store_err)?;
        let replay_key = idem_key
            .strip_prefix("vend:")
            .ok_or_else(|| VendError::Storage("invalid Vend idempotency prefix".into()))?;
        let provider: i64 = row.try_get("provider_cost_micros").map_err(store_err)?;
        let price: i64 = row.try_get("captured_charge_micros").map_err(store_err)?;
        let platform: i64 = row.try_get("platform_fee_micros").map_err(store_err)?;
        let tenant: i64 = row.try_get("tenant_net_micros").map_err(store_err)?;
        sqlx::query(
            "UPDATE metered_usage SET outcome = 'captured', provider_cost_micros = $3, \
               charge_micros = $4, platform_fee_micros = $5, tenant_net_micros = $6, \
               reconcile_lease_token = NULL, reconcile_lease_until = NULL, \
               reconcile_last_error = NULL \
             WHERE charge_id = $1 AND reconcile_lease_token = $2",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .bind(provider)
        .bind(price)
        .bind(platform)
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .map_err(store_err)?;

        if let Some(claim) = sqlx::query(
            "SELECT request_hash FROM idempotency_keys WHERE key = $1 AND state = 'pending' FOR UPDATE",
        )
        .bind(replay_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_err)?
        {
            let request_hash: String = claim.try_get("request_hash").map_err(store_err)?;
            let response = captured_recovery_replay(&request_hash, charge_id, provider, price);
            sqlx::query(
                "UPDATE idempotency_keys SET state = 'complete', response_body = $2 \
                 WHERE key = $1 AND state = 'pending'",
            )
            .bind(replay_key)
            .bind(response)
            .execute(&mut *tx)
            .await
            .map_err(store_err)?;
        }
        tx.commit().await.map_err(store_err)
    }

    pub async fn fail_attached_reconciliation(
        &self,
        charge_id: &ChargeId,
        worker_token: &str,
        error: &str,
    ) -> Result<(), VendError> {
        sqlx::query(
            "UPDATE metered_usage SET reconcile_last_error = LEFT($3, 2000) \
             WHERE charge_id = $1 AND reconcile_lease_token = $2",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(store_err)?;
        Ok(())
    }

    /// Lease incomplete payouts whose usage projection already says captured.
    pub async fn lease_pending_payouts(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
        lease_seconds: i64,
        worker_token: &str,
    ) -> Result<Vec<PayoutCandidate>, VendError> {
        validate_claim(limit, lease_seconds, worker_token)?;
        let lease_until = Utc::now() + Duration::seconds(lease_seconds);
        let rows = sqlx::query(LEASE_PAYOUT_SQL)
            .bind(older_than)
            .bind(limit)
            .bind(worker_token)
            .bind(lease_until)
            .fetch_all(self.pool())
            .await
            .map_err(store_err)?;
        rows.into_iter()
            .map(|row| {
                Ok(PayoutCandidate {
                    charge_id: ChargeId::parse(
                        &row.try_get::<String, _>("charge_id").map_err(store_err)?,
                    )?,
                    hold_id: row.try_get("hold_id").map_err(store_err)?,
                })
            })
            .collect()
    }

    pub async fn finish_payout_lease(
        &self,
        charge_id: &ChargeId,
        worker_token: &str,
        error: Option<&str>,
    ) -> Result<(), VendError> {
        sqlx::query(
            "UPDATE payout_executions SET reconcile_lease_token = NULL, \
               reconcile_lease_until = CASE WHEN $3::TEXT IS NULL THEN NULL \
                 ELSE reconcile_lease_until END, last_error = CASE WHEN $3::TEXT IS NULL \
                 THEN last_error ELSE LEFT($3, 2000) END \
             WHERE charge_id = $1 AND reconcile_lease_token = $2",
        )
        .bind(charge_id.as_str())
        .bind(worker_token)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(store_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_claims_are_bounded_and_leases_expire() {
        assert!(validate_claim(100, 120, "worker-1").is_ok());
        assert!(validate_claim(0, 120, "worker-1").is_err());
        assert!(validate_claim(1_001, 120, "worker-1").is_err());
        assert!(validate_claim(100, 29, "worker-1").is_err());
        assert!(validate_claim(100, 3_601, "worker-1").is_err());
        assert!(validate_claim(100, 120, "").is_err());
    }

    #[test]
    fn lease_queries_are_concurrent_crash_recoverable_and_money_scoped() {
        for query in [LEASE_UNATTACHED_SQL, LEASE_ATTACHED_SQL, LEASE_PAYOUT_SQL] {
            assert!(query.contains("reconcile_lease_until < now()"));
            assert!(query.contains("FOR UPDATE"));
            assert!(query.contains("SKIP LOCKED"));
            assert!(query.contains("LIMIT $2"));
        }
        assert!(LEASE_ATTACHED_SQL.contains("outcome = 'pending'"));
        assert!(LEASE_ATTACHED_SQL.contains("hold_id <> ''"));
        assert!(LEASE_ATTACHED_SQL.contains("provider_transferred OR"));
        assert!(LEASE_PAYOUT_SQL.contains("u.outcome = 'captured'"));
        assert!(LEASE_PAYOUT_SQL.contains("p.state = 'pending'"));
    }

    #[test]
    fn hold_reaper_candidate_selection_is_ttl_bounded_pending_and_capture_safe() {
        // The hold-reaper (vend-reconcile `--reap-holds`) selects its orphaned
        // candidates through LEASE_ATTACHED_SQL. Assert the safety invariants the
        // reaper relies on so a captured or fresh hold can never be leased for
        // release: (a) TTL bound via `created_at < $1`, (b) still `pending`,
        // (c) fail-closed payout signal, plus a crash-expiring lease.
        assert!(LEASE_ATTACHED_SQL.contains("u.created_at < $1")); // (a) past TTL
        assert!(LEASE_ATTACHED_SQL.contains("outcome = 'pending'")); // (b) pending
        assert!(LEASE_ATTACHED_SQL.contains("hold_id <> ''")); // has a Mint hold
        assert!(LEASE_ATTACHED_SQL.contains("payout_started")); // (c) fail-closed
        assert!(LEASE_ATTACHED_SQL.contains("reconcile_lease_until < now()"));
        assert!(LEASE_ATTACHED_SQL.contains("FOR UPDATE OF u SKIP LOCKED"));
    }

    #[test]
    fn captured_recovery_record_has_terminal_replay_shape() {
        let charge_id = ChargeId::generate();
        let raw = captured_recovery_replay("request-hash", &charge_id, 700, 1_000);
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["request_hash"], "request-hash");
        assert_eq!(value["outcome"]["status"], 409);
        assert_eq!(value["outcome"]["charge_id"], charge_id.as_str());
        assert_eq!(value["outcome"]["base_cost_micros"], 700);
        assert_eq!(value["outcome"]["price_micros"], 1_000);
        assert_eq!(
            value["outcome"]["body"]["error"]["code"],
            "settled_response_unavailable"
        );
    }
}

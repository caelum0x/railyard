//! [`InMemoryStore`] — a thread-safe in-memory implementation of the whole repo
//! port surface, so integration tests run the real domain flow with zero I/O.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use vend_core::error::VendError;
use vend_core::ids::{AppId, ChargeId, EndUserId, SubKeyId, TenantId};
use vend_core::money::Charge;
use vend_core::payout::{PayoutExecution, PayoutLeg, PayoutRepo, PayoutState};
use vend_core::ports::{
    IdempotencyClaim, IdempotencyRepo, MeteredUsage, ReadinessRepo, UsageOutcome, UsageRepo,
};
use vend_core::records::SubKeyRecord;
use vend_core::ReserveRepo;
use vend_keys::KeyRepo;
use vend_tenancy::{App, EndUser, Tenant, TenantRepo};

#[derive(Default)]
pub(crate) struct State {
    pub(crate) tenants: HashMap<String, Tenant>,
    pub(crate) apps: HashMap<String, App>,
    pub(crate) end_users: HashMap<String, EndUser>, // key: "{app_id}\u{0}{external_ref}"
    sub_keys: HashMap<String, SubKeyRecord>, // key: prefix
    pub(crate) usage: HashMap<String, MeteredUsage>, // key: charge_id
    reserve: HashMap<String, i64>,
    idempotency: HashMap<String, IdempotencyRecord>,
    pub(crate) payouts: HashMap<String, PayoutExecution>,
}

#[derive(Clone)]
struct IdempotencyRecord {
    request_hash: String,
    response: Option<String>,
}

/// An in-memory store implementing every repo port. Cheap to share via `Arc`.
#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<State>,
}

impl InMemoryStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        InMemoryStore::default()
    }

    /// Seed a tenant.
    pub fn add_tenant(&self, tenant: Tenant) {
        self.inner
            .lock()
            .unwrap()
            .tenants
            .insert(tenant.id.as_str().to_string(), tenant);
    }

    /// Seed an app.
    pub fn add_app(&self, app: App) {
        self.inner
            .lock()
            .unwrap()
            .apps
            .insert(app.id.as_str().to_string(), app);
    }

    /// Seed a provisioning-key row.
    pub fn add_sub_key(&self, key: SubKeyRecord) {
        self.inner
            .lock()
            .unwrap()
            .sub_keys
            .insert(key.prefix.clone(), key);
    }

    /// Seed a provisioned end user.
    pub fn add_end_user(&self, end_user: EndUser) {
        self.inner
            .lock()
            .unwrap()
            .end_users
            .insert(eu_key(&end_user.app_id, &end_user.external_ref), end_user);
    }

    /// Set a tenant's local reserve mirror balance.
    pub fn set_reserve(&self, tenant_id: &TenantId, micros: i64) {
        self.inner
            .lock()
            .unwrap()
            .reserve
            .insert(tenant_id.as_str().to_string(), micros);
    }

    /// Run `f` against the locked state (crate-internal repo impls).
    pub(crate) fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.inner.lock().unwrap())
    }

    /// Read one metered usage row for integration assertions.
    pub fn usage_by_charge_id(&self, charge_id: &str) -> Option<MeteredUsage> {
        self.inner.lock().unwrap().usage.get(charge_id).cloned()
    }

    /// Read one payout execution for integration assertions.
    pub fn payout_by_charge_id(&self, charge_id: &str) -> Option<PayoutExecution> {
        self.inner.lock().unwrap().payouts.get(charge_id).cloned()
    }
}

fn eu_key(app_id: &AppId, external_ref: &str) -> String {
    format!("{}\u{0}{}", app_id.as_str(), external_ref)
}

#[async_trait]
impl TenantRepo for InMemoryStore {
    async fn find_sub_key_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<SubKeyRecord>, VendError> {
        Ok(self.inner.lock().unwrap().sub_keys.get(prefix).cloned())
    }

    async fn load_tenant(&self, id: &TenantId) -> Result<Option<Tenant>, VendError> {
        Ok(self.inner.lock().unwrap().tenants.get(id.as_str()).cloned())
    }

    async fn load_app(&self, id: &AppId) -> Result<Option<App>, VendError> {
        Ok(self.inner.lock().unwrap().apps.get(id.as_str()).cloned())
    }

    async fn find_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
    ) -> Result<Option<EndUser>, VendError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .end_users
            .get(&eu_key(app_id, external_ref))
            .cloned())
    }

    async fn load_end_user(
        &self,
        app_id: &AppId,
        id: &EndUserId,
    ) -> Result<Option<EndUser>, VendError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .end_users
            .values()
            .find(|end_user| end_user.app_id == *app_id && end_user.id == *id)
            .cloned())
    }

    async fn create_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
        mint_account_id: &str,
    ) -> Result<EndUser, VendError> {
        let eu = EndUser {
            id: EndUserId::generate(),
            app_id: app_id.clone(),
            external_ref: external_ref.to_string(),
            mint_account_id: Some(mint_account_id.to_string()),
        };
        self.inner
            .lock()
            .unwrap()
            .end_users
            .insert(eu_key(app_id, external_ref), eu.clone());
        Ok(eu)
    }
}

#[async_trait]
impl KeyRepo for InMemoryStore {
    async fn insert(&self, record: &SubKeyRecord) -> Result<(), VendError> {
        self.inner
            .lock()
            .unwrap()
            .sub_keys
            .insert(record.prefix.clone(), record.clone());
        Ok(())
    }

    async fn find_by_prefix(&self, prefix: &str) -> Result<Option<SubKeyRecord>, VendError> {
        Ok(self.inner.lock().unwrap().sub_keys.get(prefix).cloned())
    }

    async fn revoke(&self, id: &SubKeyId) -> Result<(), VendError> {
        let mut s = self.inner.lock().unwrap();
        for key in s.sub_keys.values_mut() {
            if &key.id == id {
                key.status = vend_core::records::KeyStatus::Revoked;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl UsageRepo for InMemoryStore {
    async fn record_pending(&self, usage: &MeteredUsage) -> Result<(), VendError> {
        self.inner
            .lock()
            .unwrap()
            .usage
            .insert(usage.charge_id.as_str().to_string(), usage.clone());
        Ok(())
    }

    async fn record_pending_within_cap(
        &self,
        usage: &MeteredUsage,
        daily_cap_micros: Option<i64>,
        per_end_user_daily_cap_micros: Option<i64>,
    ) -> Result<(), VendError> {
        // Hold the mutex across the whole check-then-insert so concurrent
        // reservations serialize — the same atomicity the Postgres store gets
        // from a locked transaction. This is the authoritative TOCTOU-safe gate.
        let mut s = self.inner.lock().unwrap();

        if let Some(cap) = daily_cap_micros {
            let spent: i64 = s
                .usage
                .values()
                .filter(|u| {
                    u.sub_key_id == usage.sub_key_id
                        && u.utc_day == usage.utc_day
                        && u.outcome != UsageOutcome::Released
                })
                .map(|u| u.charge_micros)
                .sum();
            if spent.saturating_add(usage.charge_micros) > cap {
                return Err(VendError::SpendCapExceeded);
            }
        }

        if let (Some(cap), Some(eu)) = (per_end_user_daily_cap_micros, usage.end_user_id.as_ref()) {
            let spent: i64 = s
                .usage
                .values()
                .filter(|u| {
                    u.end_user_id.as_ref() == Some(eu)
                        && u.utc_day == usage.utc_day
                        && u.outcome != UsageOutcome::Released
                })
                .map(|u| u.charge_micros)
                .sum();
            if spent.saturating_add(usage.charge_micros) > cap {
                return Err(VendError::SpendCapExceeded);
            }
        }

        s.usage
            .insert(usage.charge_id.as_str().to_string(), usage.clone());
        Ok(())
    }

    async fn attach_hold(&self, charge_id: &ChargeId, hold_id: &str) -> Result<(), VendError> {
        if hold_id.is_empty() {
            return Err(VendError::Storage("empty hold id".to_string()));
        }
        let mut state = self.inner.lock().unwrap();
        let usage = state
            .usage
            .get_mut(charge_id.as_str())
            .ok_or_else(|| VendError::Storage("missing pending usage intent".to_string()))?;
        if usage.outcome != UsageOutcome::Pending || !usage.hold_id.is_empty() {
            return Err(VendError::Storage(
                "pending usage intent already has a hold".to_string(),
            ));
        }
        usage.hold_id = hold_id.to_string();
        Ok(())
    }

    async fn mark_captured(&self, charge_id: &ChargeId, actual: &Charge) -> Result<(), VendError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(u) = s.usage.get_mut(charge_id.as_str()) {
            u.outcome = UsageOutcome::Captured;
            u.provider_cost_micros = actual.provider_cost_micros;
            u.charge_micros = actual.charge_micros;
            u.platform_fee_micros = actual.platform_fee_micros;
            u.tenant_net_micros = actual.tenant_net_micros;
        }
        Ok(())
    }

    async fn mark_released(&self, charge_id: &ChargeId) -> Result<(), VendError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(u) = s.usage.get_mut(charge_id.as_str()) {
            u.outcome = UsageOutcome::Released;
        }
        Ok(())
    }

    async fn record_settlement_proof(
        &self,
        charge_id: &ChargeId,
        proof: &vend_core::transport::SettlementProof,
    ) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        let usage = state
            .usage
            .get_mut(charge_id.as_str())
            .ok_or_else(|| VendError::Storage("missing usage for settlement proof".to_string()))?;
        if usage.outcome == UsageOutcome::Pending {
            return Err(VendError::Storage(
                "cannot receipt a pending usage row".to_string(),
            ));
        }
        match &usage.settlement_proof {
            Some(existing) if existing != proof => Err(VendError::Storage(
                "conflicting settlement proof".to_string(),
            )),
            Some(_) => Ok(()),
            None => {
                usage.settlement_proof = Some(proof.clone());
                Ok(())
            }
        }
    }

    async fn sum_charge_for_key_day(
        &self,
        sub_key_id: &SubKeyId,
        utc_day: &str,
    ) -> Result<i64, VendError> {
        let s = self.inner.lock().unwrap();
        Ok(s.usage
            .values()
            .filter(|u| {
                &u.sub_key_id == sub_key_id
                    && u.utc_day == utc_day
                    && u.outcome != UsageOutcome::Released
            })
            .map(|u| u.charge_micros)
            .sum())
    }

    async fn sum_charge_for_end_user_day(
        &self,
        end_user_id: &EndUserId,
        utc_day: &str,
    ) -> Result<i64, VendError> {
        let s = self.inner.lock().unwrap();
        Ok(s.usage
            .values()
            .filter(|u| {
                u.end_user_id.as_ref() == Some(end_user_id)
                    && u.utc_day == utc_day
                    && u.outcome != UsageOutcome::Released
            })
            .map(|u| u.charge_micros)
            .sum())
    }
}

#[async_trait]
impl PayoutRepo for InMemoryStore {
    async fn prepare_payout(&self, payout: &PayoutExecution) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        match state.payouts.get(payout.charge_id.as_str()) {
            Some(existing) => {
                let mut expected = payout.clone();
                expected.provider_transferred = existing.provider_transferred;
                expected.tenant_transferred = existing.tenant_transferred;
                expected.platform_transferred = existing.platform_transferred;
                expected.state = existing.state;
                expected.last_error.clone_from(&existing.last_error);
                if existing == &expected {
                    Ok(())
                } else {
                    Err(VendError::IdempotencyConflict)
                }
            }
            None => {
                state
                    .payouts
                    .insert(payout.charge_id.as_str().to_string(), payout.clone());
                Ok(())
            }
        }
    }

    async fn load_payout(
        &self,
        charge_id: &ChargeId,
    ) -> Result<Option<PayoutExecution>, VendError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .payouts
            .get(charge_id.as_str())
            .cloned())
    }

    async fn pending_payouts_before(
        &self,
        _older_than_unix_seconds: i64,
        limit: u32,
    ) -> Result<Vec<PayoutExecution>, VendError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .payouts
            .values()
            .filter(|payout| payout.state == PayoutState::Pending)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn mark_payout_leg(&self, charge_id: &ChargeId, leg: PayoutLeg) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        let payout = state
            .payouts
            .get_mut(charge_id.as_str())
            .ok_or(VendError::NotFound)?;
        match leg {
            PayoutLeg::Provider => payout.provider_transferred = true,
            PayoutLeg::Tenant => payout.tenant_transferred = true,
            PayoutLeg::Platform => payout.platform_transferred = true,
        }
        payout.last_error = None;
        Ok(())
    }

    async fn mark_payout_complete(&self, charge_id: &ChargeId) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        let payout = state
            .payouts
            .get_mut(charge_id.as_str())
            .ok_or(VendError::NotFound)?;
        if !(payout.provider_transferred
            && payout.tenant_transferred
            && payout.platform_transferred)
        {
            return Err(VendError::Storage("payout legs remain".into()));
        }
        payout.state = PayoutState::Complete;
        payout.last_error = None;
        Ok(())
    }

    async fn record_payout_error(
        &self,
        charge_id: &ChargeId,
        message: &str,
    ) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        let payout = state
            .payouts
            .get_mut(charge_id.as_str())
            .ok_or(VendError::NotFound)?;
        payout.last_error = Some(message.to_string());
        Ok(())
    }
}

#[async_trait]
impl ReserveRepo for InMemoryStore {
    async fn tenant_reserve_balance(&self, tenant_id: &TenantId) -> Result<i64, VendError> {
        Ok(*self
            .inner
            .lock()
            .unwrap()
            .reserve
            .get(tenant_id.as_str())
            .unwrap_or(&0))
    }
}

#[async_trait]
impl ReadinessRepo for InMemoryStore {
    async fn ready(&self) -> Result<(), VendError> {
        Ok(())
    }
}

#[async_trait]
impl IdempotencyRepo for InMemoryStore {
    async fn claim(&self, key: &str, request_hash: &str) -> Result<IdempotencyClaim, VendError> {
        let mut state = self.inner.lock().unwrap();
        match state.idempotency.get(key) {
            Some(record) if record.request_hash != request_hash => Ok(IdempotencyClaim::Conflict),
            Some(record) => match &record.response {
                Some(response) => Ok(IdempotencyClaim::Replay(response.clone())),
                None => Ok(IdempotencyClaim::InProgress),
            },
            None => {
                state.idempotency.insert(
                    key.to_string(),
                    IdempotencyRecord {
                        request_hash: request_hash.to_string(),
                        response: None,
                    },
                );
                Ok(IdempotencyClaim::Acquired)
            }
        }
    }

    async fn complete(
        &self,
        key: &str,
        request_hash: &str,
        response_body: &str,
    ) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        let record = state
            .idempotency
            .get_mut(key)
            .ok_or(VendError::IdempotencyConflict)?;
        if record.request_hash != request_hash || record.response.is_some() {
            return Err(VendError::IdempotencyConflict);
        }
        record.response = Some(response_body.to_string());
        Ok(())
    }

    async fn abort(&self, key: &str, request_hash: &str) -> Result<(), VendError> {
        let mut state = self.inner.lock().unwrap();
        if state
            .idempotency
            .get(key)
            .is_some_and(|record| record.request_hash == request_hash && record.response.is_none())
        {
            state.idempotency.remove(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn concurrent_idempotency_claim_has_one_owner() {
        let store = Arc::new(InMemoryStore::new());
        let (left, right) = tokio::join!(store.claim("key", "hash"), store.claim("key", "hash"));
        let claims = [left.unwrap(), right.unwrap()];
        assert_eq!(
            claims
                .iter()
                .filter(|claim| **claim == IdempotencyClaim::Acquired)
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| **claim == IdempotencyClaim::InProgress)
                .count(),
            1
        );

        store.complete("key", "hash", "response").await.unwrap();
        assert_eq!(
            store.claim("key", "hash").await.unwrap(),
            IdempotencyClaim::Replay("response".to_string())
        );
        assert_eq!(
            store.claim("key", "different").await.unwrap(),
            IdempotencyClaim::Conflict
        );
    }
}

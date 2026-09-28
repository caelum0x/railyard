//! Spend caps with in-flight reservations.
//!
//! The check is `spent (from the audit log) + reserved (in-flight holds) +
//! this quote <= limit` for every applicable cap. A passing check reserves the
//! quote until the returned [`CapGuard`] drops (after the call row is written).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use switchyard_core::ApiError;
use switchyard_store::{ApiKey, Project, SpendFilter, Store};

#[derive(Default)]
pub struct Caps {
    reserved: Mutex<HashMap<String, i64>>,
}

pub struct CapGuard {
    caps: Arc<Caps>,
    entries: Vec<(String, i64)>,
}

impl Drop for CapGuard {
    fn drop(&mut self) {
        let mut r = self.caps.reserved.lock().expect("caps poisoned");
        for (k, v) in &self.entries {
            if let Some(x) = r.get_mut(k) {
                *x -= v;
                if *x <= 0 {
                    r.remove(k);
                }
            }
        }
    }
}

pub fn start_of_day(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0).single().unwrap_or(now)
}

pub fn start_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).single().unwrap_or(now)
}

struct Check {
    name: &'static str,
    reserve_key: String,
    limit: i64,
    filter: SpendFilter,
    amount: i64,
}

impl Caps {
    pub fn new() -> Arc<Self> {
        Arc::new(Caps::default())
    }

    pub fn reserved(&self, key: &str) -> i64 {
        *self.reserved.lock().expect("caps poisoned").get(key).unwrap_or(&0)
    }

    /// Check every cap for a call quoted at `quote` micros and reserve it.
    /// `send` calls also count against the project's daily send limit.
    pub async fn check_and_reserve(
        self: &Arc<Self>,
        store: &dyn Store,
        project: &Project,
        key: &ApiKey,
        target: &str,
        is_send: bool,
        quote: i64,
        now: DateTime<Utc>,
    ) -> Result<CapGuard, ApiError> {
        let day = start_of_day(now);
        let month = start_of_month(now);
        let base = SpendFilter { project_id: project.id.clone(), ..Default::default() };
        let mut checks = Vec::new();
        if let Some(limit) = project.monthly_cap_micros {
            checks.push(Check { name: "project_monthly", reserve_key: format!("m:{}", project.id), limit, filter: SpendFilter { since: Some(month), ..base.clone() }, amount: quote });
        }
        if let Some(limit) = project.daily_cap_micros {
            checks.push(Check { name: "project_daily", reserve_key: format!("d:{}", project.id), limit, filter: SpendFilter { since: Some(day), ..base.clone() }, amount: quote });
        }
        if let Some(limit) = key.per_key_daily_cap_micros {
            checks.push(Check {
                name: "key_daily",
                reserve_key: format!("k:{}", key.public_id),
                limit,
                filter: SpendFilter { since: Some(day), key_public_id: Some(key.public_id.clone()), ..base.clone() },
                amount: quote,
            });
        }
        if let Some(limit) = project.tool_daily_caps.get(target) {
            checks.push(Check {
                name: "tool_daily",
                reserve_key: format!("t:{}:{target}", project.id),
                limit: *limit,
                filter: SpendFilter { since: Some(day), target: Some(target.to_string()), ..base.clone() },
                amount: quote,
            });
        }
        if is_send {
            if let Some(limit) = project.daily_send_limit {
                checks.push(Check {
                    name: "daily_sends",
                    reserve_key: format!("s:{}", project.id),
                    limit,
                    filter: SpendFilter { since: Some(day), side_effect: Some("send".into()), count_calls: true, ..base.clone() },
                    amount: 1,
                });
            }
        }
        // Zero-cost calls only need the count-based send limit.
        let checks: Vec<Check> = checks.into_iter().filter(|c| c.amount > 0).collect();
        let mut spent = Vec::with_capacity(checks.len());
        for c in &checks {
            spent.push(store.spend_since(c.filter.clone()).await.map_err(|e| ApiError::internal(e.to_string()))?);
        }
        let mut r = self.reserved.lock().expect("caps poisoned");
        for (c, s) in checks.iter().zip(spent.iter()) {
            let in_flight = *r.get(&c.reserve_key).unwrap_or(&0);
            if s + in_flight + c.amount > c.limit {
                return Err(ApiError::cap_exceeded(c.name, c.limit, s + in_flight, c.amount));
            }
        }
        let mut entries = Vec::new();
        for c in &checks {
            *r.entry(c.reserve_key.clone()).or_default() += c.amount;
            entries.push((c.reserve_key.clone(), c.amount));
        }
        Ok(CapGuard { caps: self.clone(), entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use switchyard_store::{MemStore, NewCall};

    fn project() -> Project {
        Project {
            id: "prj".into(),
            org_id: "org".into(),
            name: "p".into(),
            monthly_cap_micros: Some(1_000),
            daily_cap_micros: Some(500),
            tool_daily_caps: [("rally.email.send".to_string(), 300)].into(),
            routing_policy: "free_first".into(),
            egress_extra_hosts: vec![],
            pinned_tools: vec![],
            daily_send_limit: Some(2),
            created_at: Utc::now(),
        }
    }

    fn key(cap: Option<i64>) -> ApiKey {
        ApiKey {
            public_id: "kkkkkkkkkkkk".into(),
            project_id: "prj".into(),
            description: String::new(),
            scopes: vec![],
            tool_allow: None,
            per_key_daily_cap_micros: cap,
            extra_projects: vec![],
            revoked_at: None,
            created_at: Utc::now(),
        }
    }

    async fn spend(store: &MemStore, id: &str, cost: i64, target: &str, side: &str) {
        store
            .record_call(
                NewCall {
                    id: id.into(),
                    project_id: "prj".into(),
                    org_id: "org".into(),
                    key_public_id: "kkkkkkkkkkkk".into(),
                    target: target.into(),
                    tool_id: Some(target.into()),
                    provider_id: None,
                    side_effect: side.into(),
                    idempotency_key: None,
                    request_hash: "h".into(),
                    status: "ok".into(),
                    http_status: 200,
                    latency_ms: 1,
                    cost_micros: cost,
                    price_basis: "per_call".into(),
                    mint_hold_id: None,
                    mint_receipt: None,
                    rago_event_id: None,
                    cache: "miss".into(),
                    fallback_chain: json!([]),
                    created_at: Utc::now(),
                },
                vec![],
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn daily_cap_counts_spend_and_reservations() {
        let store = MemStore::new();
        let caps = Caps::new();
        let p = project();
        spend(&store, "c1", 400, "weather.forecast", "read").await;
        let g = caps.check_and_reserve(&store, &p, &key(None), "x", false, 100, Utc::now()).await.unwrap();
        let err = caps.check_and_reserve(&store, &p, &key(None), "x", false, 1, Utc::now()).await.err().unwrap();
        assert_eq!(err.status, 429);
        assert_eq!(err.details["cap"], "project_daily");
        drop(g);
        assert_eq!(caps.reserved("d:prj"), 0, "guard releases reservation");
        assert!(caps.check_and_reserve(&store, &p, &key(None), "x", false, 100, Utc::now()).await.is_ok());
    }

    #[tokio::test]
    async fn key_and_tool_caps_and_send_limit() {
        let store = MemStore::new();
        let caps = Caps::new();
        let p = project();
        let e = caps.check_and_reserve(&store, &p, &key(Some(50)), "x", false, 60, Utc::now()).await.err().unwrap();
        assert_eq!(e.details["cap"], "key_daily");
        let e = caps.check_and_reserve(&store, &p, &key(None), "rally.email.send", true, 301, Utc::now()).await.err().unwrap();
        assert_eq!(e.details["cap"], "tool_daily");
        spend(&store, "s1", 0, "rally.email.send", "send").await;
        spend(&store, "s2", 0, "rally.email.send", "send").await;
        let e = caps.check_and_reserve(&store, &p, &key(None), "rally.email.send", true, 0, Utc::now()).await.err().unwrap();
        assert_eq!(e.details["cap"], "daily_sends");
        assert!(caps.check_and_reserve(&store, &p, &key(None), "weather.forecast", false, 0, Utc::now()).await.is_ok(), "free reads are never capped");
    }
}

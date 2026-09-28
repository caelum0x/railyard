//! In-memory [`Store`] (tests and `SWITCHYARD_STORE=memory` dev runs).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use switchyard_core::hash::{chain_hash, GENESIS_HASH};

use crate::models::*;
use crate::{Store, StoreError, StoreResult};

#[derive(Default)]
struct State {
    orgs: BTreeMap<String, Org>,
    projects: BTreeMap<String, Project>,
    members: HashMap<(String, String), Member>,
    keys: BTreeMap<String, ApiKey>,
    org_keys: BTreeMap<String, OrgKey>,
    credentials: Vec<CredentialRow>,
    bundles: BTreeMap<(String, String), BundleRow>,
    byo: BTreeMap<(String, String), ByoTool>,
    calls: HashMap<String, Vec<CallRow>>,
    idem: HashMap<(String, String), IdempotencyRow>,
    outbox: Vec<OutboxItem>,
    stats: BTreeMap<String, ToolStat>,
    clients: HashMap<String, OAuthClient>,
    codes: HashMap<String, OAuthCode>,
    grants: HashMap<String, OAuthGrant>,
    revoked_families: HashSet<String>,
    consents: HashSet<(String, String)>,
    devices: HashMap<String, DeviceCode>,
}

#[derive(Default)]
pub struct MemStore {
    s: Mutex<State>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.s.lock().expect("mem store poisoned")
    }

    /// Test helper: pending outbox items.
    pub fn outbox_len(&self) -> usize {
        self.lock().outbox.iter().filter(|o| o.delivered_at.is_none()).count()
    }
}

fn nf(what: &str) -> StoreError {
    StoreError::NotFound(what.to_string())
}

fn matches(c: &NewCall, f: &SpendFilter) -> bool {
    f.since.map(|s| c.created_at >= s).unwrap_or(true)
        && f.key_public_id.as_ref().map(|k| &c.key_public_id == k).unwrap_or(true)
        && f.target.as_ref().map(|t| &c.target == t || c.tool_id.as_ref() == Some(t)).unwrap_or(true)
        && f.side_effect.as_ref().map(|s| &c.side_effect == s).unwrap_or(true)
}

#[async_trait]
impl Store for MemStore {
    async fn create_org(&self, org: Org) -> StoreResult<Org> {
        let mut s = self.lock();
        if s.orgs.values().any(|o| o.slug == org.slug) {
            return Err(StoreError::Conflict(format!("org slug {}", org.slug)));
        }
        s.orgs.insert(org.id.clone(), org.clone());
        Ok(org)
    }
    async fn get_org(&self, id: &str) -> StoreResult<Org> {
        self.lock().orgs.get(id).cloned().ok_or_else(|| nf("org"))
    }
    async fn list_orgs(&self) -> StoreResult<Vec<Org>> {
        Ok(self.lock().orgs.values().cloned().collect())
    }
    async fn create_project(&self, p: Project) -> StoreResult<Project> {
        self.lock().projects.insert(p.id.clone(), p.clone());
        Ok(p)
    }
    async fn get_project(&self, id: &str) -> StoreResult<Project> {
        self.lock().projects.get(id).cloned().ok_or_else(|| nf("project"))
    }
    async fn update_project(&self, p: Project) -> StoreResult<Project> {
        let mut s = self.lock();
        if !s.projects.contains_key(&p.id) {
            return Err(nf("project"));
        }
        s.projects.insert(p.id.clone(), p.clone());
        Ok(p)
    }
    async fn list_projects(&self, org_id: &str) -> StoreResult<Vec<Project>> {
        Ok(self.lock().projects.values().filter(|p| p.org_id == org_id).cloned().collect())
    }
    async fn upsert_member(&self, m: Member) -> StoreResult<()> {
        self.lock().members.insert((m.org_id.clone(), m.user_id.clone()), m);
        Ok(())
    }
    async fn get_member(&self, org_id: &str, user_id: &str) -> StoreResult<Option<Member>> {
        Ok(self.lock().members.get(&(org_id.to_string(), user_id.to_string())).cloned())
    }

    async fn insert_api_key(&self, k: ApiKey) -> StoreResult<ApiKey> {
        let mut s = self.lock();
        if s.keys.contains_key(&k.public_id) {
            return Err(StoreError::Conflict("api key".into()));
        }
        s.keys.insert(k.public_id.clone(), k.clone());
        Ok(k)
    }
    async fn get_api_key(&self, public_id: &str) -> StoreResult<Option<ApiKey>> {
        Ok(self.lock().keys.get(public_id).cloned())
    }
    async fn list_api_keys(&self, project_id: &str) -> StoreResult<Vec<ApiKey>> {
        Ok(self.lock().keys.values().filter(|k| k.project_id == project_id).cloned().collect())
    }
    async fn update_api_key(&self, k: ApiKey) -> StoreResult<ApiKey> {
        let mut s = self.lock();
        if !s.keys.contains_key(&k.public_id) {
            return Err(nf("api key"));
        }
        s.keys.insert(k.public_id.clone(), k.clone());
        Ok(k)
    }

    async fn ensure_org_key(&self, k: OrgKey) -> StoreResult<OrgKey> {
        let mut s = self.lock();
        Ok(s.org_keys.entry(k.org_id.clone()).or_insert(k).clone())
    }
    async fn get_org_key(&self, org_id: &str) -> StoreResult<Option<OrgKey>> {
        Ok(self.lock().org_keys.get(org_id).cloned())
    }
    async fn list_org_keys(&self) -> StoreResult<Vec<OrgKey>> {
        Ok(self.lock().org_keys.values().cloned().collect())
    }
    async fn update_org_key(&self, k: OrgKey) -> StoreResult<()> {
        self.lock().org_keys.insert(k.org_id.clone(), k);
        Ok(())
    }
    async fn insert_credential(&self, c: CredentialRow) -> StoreResult<CredentialRow> {
        self.lock().credentials.push(c.clone());
        Ok(c)
    }
    async fn get_credential(&self, org_id: &str, id: &str) -> StoreResult<Option<CredentialRow>> {
        Ok(self.lock().credentials.iter().find(|c| c.org_id == org_id && c.id == id).cloned())
    }
    async fn list_credentials(&self, org_id: &str) -> StoreResult<Vec<CredentialRow>> {
        Ok(self.lock().credentials.iter().filter(|c| c.org_id == org_id).cloned().collect())
    }
    async fn find_credential(&self, org_id: &str, provider_id: &str) -> StoreResult<Option<CredentialRow>> {
        Ok(self
            .lock()
            .credentials
            .iter()
            .filter(|c| c.org_id == org_id && c.provider_id == provider_id)
            .max_by_key(|c| (c.created_at, c.id.clone()))
            .cloned())
    }
    async fn delete_credential(&self, org_id: &str, id: &str) -> StoreResult<bool> {
        let mut s = self.lock();
        let before = s.credentials.len();
        s.credentials.retain(|c| !(c.org_id == org_id && c.id == id));
        Ok(s.credentials.len() != before)
    }
    async fn mark_credential(&self, id: &str, ok: bool, error: Option<String>) -> StoreResult<()> {
        let mut s = self.lock();
        if let Some(c) = s.credentials.iter_mut().find(|c| c.id == id) {
            if ok {
                c.last_ok_at = Some(Utc::now());
                c.last_error = None;
            } else {
                c.last_error = error;
            }
        }
        Ok(())
    }

    async fn upsert_bundle(&self, b: BundleRow) -> StoreResult<BundleRow> {
        let mut s = self.lock();
        let key = (b.org_id.clone(), b.id.clone());
        let mut b = b;
        if let Some(prev) = s.bundles.get(&key) {
            b.version = prev.version + 1;
        }
        s.bundles.insert(key, b.clone());
        Ok(b)
    }
    async fn list_bundles(&self, org_id: &str) -> StoreResult<Vec<BundleRow>> {
        Ok(self.lock().bundles.values().filter(|b| b.org_id == org_id).cloned().collect())
    }
    async fn get_bundle(&self, org_id: &str, id: &str) -> StoreResult<Option<BundleRow>> {
        Ok(self.lock().bundles.get(&(org_id.to_string(), id.to_string())).cloned())
    }

    async fn upsert_byo_tool(&self, t: ByoTool) -> StoreResult<ByoTool> {
        self.lock().byo.insert((t.org_id.clone(), t.id.clone()), t.clone());
        Ok(t)
    }
    async fn get_byo_tool(&self, org_id: &str, id: &str) -> StoreResult<Option<ByoTool>> {
        Ok(self.lock().byo.get(&(org_id.to_string(), id.to_string())).cloned())
    }
    async fn list_byo_tools(&self, org_id: Option<&str>) -> StoreResult<Vec<ByoTool>> {
        Ok(self.lock().byo.values().filter(|t| org_id.map(|o| t.org_id == o).unwrap_or(true)).cloned().collect())
    }

    async fn record_call(&self, call: NewCall, outbox: Vec<OutboxItem>, idem: Option<IdempotencyRow>) -> StoreResult<CallRow> {
        let mut s = self.lock();
        let rows = s.calls.entry(call.project_id.clone()).or_default();
        let seq = rows.len() as i64 + 1;
        let prev_hash = rows.last().map(|r| r.row_hash.clone()).unwrap_or_else(|| GENESIS_HASH.to_string());
        let row_hash = chain_hash(&prev_hash, &call.hash_payload(seq));
        let row = CallRow { call, seq, prev_hash, row_hash };
        rows.push(row.clone());
        s.outbox.extend(outbox);
        if let Some(i) = idem {
            s.idem.insert((i.project_id.clone(), i.key.clone()), i);
        }
        Ok(row)
    }
    async fn get_call(&self, project_id: &str, id: &str) -> StoreResult<Option<CallRow>> {
        Ok(self.lock().calls.get(project_id).and_then(|rows| rows.iter().find(|r| r.call.id == id).cloned()))
    }
    async fn list_calls(&self, project_id: &str, before: Option<&str>, limit: i64) -> StoreResult<Vec<CallRow>> {
        let s = self.lock();
        let rows = match s.calls.get(project_id) {
            Some(r) => r,
            None => return Ok(vec![]),
        };
        let upto = before.and_then(|b| rows.iter().find(|r| r.call.id == b)).map(|r| r.seq).unwrap_or(i64::MAX);
        Ok(rows.iter().rev().filter(|r| r.seq < upto).take(limit.max(0) as usize).cloned().collect())
    }
    async fn chain(&self, project_id: &str, upto_seq: i64) -> StoreResult<Vec<CallRow>> {
        Ok(self.lock().calls.get(project_id).map(|rows| rows.iter().filter(|r| r.seq <= upto_seq).cloned().collect()).unwrap_or_default())
    }
    async fn spend_since(&self, f: SpendFilter) -> StoreResult<i64> {
        let s = self.lock();
        let rows = match s.calls.get(&f.project_id) {
            Some(r) => r,
            None => return Ok(0),
        };
        let it = rows.iter().map(|r| &r.call).filter(|c| matches(c, &f));
        Ok(if f.count_calls { it.filter(|c| c.status == "ok" && c.cache != "replay").count() as i64 } else { it.map(|c| c.cost_micros).sum() })
    }
    async fn usage(&self, project_id: &str, from: DateTime<Utc>, to: DateTime<Utc>, group_by: GroupBy) -> StoreResult<Vec<UsageRow>> {
        let s = self.lock();
        let mut groups: BTreeMap<String, UsageRow> = BTreeMap::new();
        for r in s.calls.get(project_id).into_iter().flatten() {
            let c = &r.call;
            if c.created_at < from || c.created_at >= to {
                continue;
            }
            let g = match group_by {
                GroupBy::Tool => c.tool_id.clone().unwrap_or_else(|| c.target.clone()),
                GroupBy::Provider => c.provider_id.clone().unwrap_or_else(|| "-".into()),
                GroupBy::Key => c.key_public_id.clone(),
                GroupBy::Day => c.created_at.format("%Y-%m-%d").to_string(),
            };
            let e = groups.entry(g.clone()).or_insert(UsageRow { group: g, calls: 0, billed_calls: 0, cost_micros: 0 });
            e.calls += 1;
            if c.cost_micros > 0 {
                e.billed_calls += 1;
            }
            e.cost_micros += c.cost_micros;
        }
        Ok(groups.into_values().collect())
    }
    async fn get_idempotency(&self, project_id: &str, key: &str, now: DateTime<Utc>) -> StoreResult<Option<IdempotencyRow>> {
        Ok(self.lock().idem.get(&(project_id.to_string(), key.to_string())).filter(|i| i.expires_at > now).cloned())
    }
    async fn due_outbox(&self, now: DateTime<Utc>, limit: i64) -> StoreResult<Vec<OutboxItem>> {
        Ok(self.lock().outbox.iter().filter(|o| o.delivered_at.is_none() && o.next_attempt_at <= now).take(limit.max(0) as usize).cloned().collect())
    }
    async fn mark_outbox(&self, id: &str, delivered: bool, error: Option<String>, next_attempt_at: DateTime<Utc>) -> StoreResult<()> {
        let mut s = self.lock();
        if let Some(o) = s.outbox.iter_mut().find(|o| o.id == id) {
            o.attempts += 1;
            if delivered {
                o.delivered_at = Some(Utc::now());
            } else {
                o.last_error = error;
                o.next_attempt_at = next_attempt_at;
            }
        }
        Ok(())
    }
    async fn record_stat(&self, tool_id: &str, success: bool, latency_ms: i64, cost_micros: i64) -> StoreResult<()> {
        let mut s = self.lock();
        let e = s.stats.entry(tool_id.to_string()).or_insert(ToolStat {
            tool_id: tool_id.to_string(),
            calls: 0,
            successes: 0,
            total_latency_ms: 0,
            observed_cost_micros: 0,
        });
        e.calls += 1;
        e.successes += success as i64;
        e.total_latency_ms += latency_ms;
        e.observed_cost_micros += cost_micros;
        Ok(())
    }
    async fn stats(&self) -> StoreResult<Vec<ToolStat>> {
        Ok(self.lock().stats.values().cloned().collect())
    }

    async fn insert_client(&self, c: OAuthClient) -> StoreResult<OAuthClient> {
        self.lock().clients.insert(c.client_id.clone(), c.clone());
        Ok(c)
    }
    async fn get_client(&self, client_id: &str) -> StoreResult<Option<OAuthClient>> {
        Ok(self.lock().clients.get(client_id).cloned())
    }
    async fn insert_code(&self, c: OAuthCode) -> StoreResult<()> {
        self.lock().codes.insert(c.code_hash.clone(), c);
        Ok(())
    }
    async fn take_code(&self, code_hash: &str) -> StoreResult<Option<OAuthCode>> {
        Ok(self.lock().codes.remove(code_hash))
    }
    async fn insert_grant(&self, g: OAuthGrant) -> StoreResult<()> {
        self.lock().grants.insert(g.id.clone(), g);
        Ok(())
    }
    async fn find_grant_by_refresh(&self, refresh_hash: &str) -> StoreResult<Option<OAuthGrant>> {
        Ok(self.lock().grants.values().find(|g| g.refresh_hash == refresh_hash).cloned())
    }
    async fn consume_grant(&self, id: &str) -> StoreResult<bool> {
        let mut s = self.lock();
        match s.grants.get_mut(id) {
            Some(g) if g.used_at.is_none() => {
                g.used_at = Some(Utc::now());
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Err(nf("grant")),
        }
    }
    async fn revoke_family(&self, family_id: &str) -> StoreResult<()> {
        self.lock().revoked_families.insert(family_id.to_string());
        Ok(())
    }
    async fn family_revoked(&self, family_id: &str) -> StoreResult<bool> {
        Ok(self.lock().revoked_families.contains(family_id))
    }
    async fn add_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<()> {
        self.lock().consents.insert((family_id.to_string(), tool_id.to_string()));
        Ok(())
    }
    async fn has_consent(&self, family_id: &str, tool_id: &str) -> StoreResult<bool> {
        Ok(self.lock().consents.contains(&(family_id.to_string(), tool_id.to_string())))
    }
    async fn insert_device(&self, d: DeviceCode) -> StoreResult<()> {
        self.lock().devices.insert(d.device_hash.clone(), d);
        Ok(())
    }
    async fn get_device_by_user_code(&self, user_code: &str) -> StoreResult<Option<DeviceCode>> {
        Ok(self.lock().devices.values().find(|d| d.user_code == user_code).cloned())
    }
    async fn get_device(&self, device_hash: &str) -> StoreResult<Option<DeviceCode>> {
        Ok(self.lock().devices.get(device_hash).cloned())
    }
    async fn update_device(&self, d: DeviceCode) -> StoreResult<()> {
        self.lock().devices.insert(d.device_hash.clone(), d);
        Ok(())
    }
}

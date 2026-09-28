//! Row types.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Org {
    pub id: String,
    pub slug: String,
    pub name: String,
    /// Mint account (`acct_...`) that funds this org's paid calls.
    pub mint_account_id: Option<String>,
    /// Rago `external_subscription_id` that receives `tool_call` events.
    pub rago_subscription_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub monthly_cap_micros: Option<i64>,
    pub daily_cap_micros: Option<i64>,
    /// Per-tool daily sub-caps.
    pub tool_daily_caps: BTreeMap<String, i64>,
    /// `free_first` | `cheapest` | `fastest` | `pinned`
    pub routing_policy: String,
    pub egress_extra_hosts: Vec<String>,
    /// Up to 20 tools exposed as first-class MCP tools.
    pub pinned_tools: Vec<String>,
    /// Daily cap on `send` side-effect calls (abuse guard for email/magic links).
    pub daily_send_limit: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub org_id: String,
    pub user_id: String,
    /// owner | admin | member | viewer
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKey {
    /// Relay key public id (the 12 chars in `sk-t0-<public_id>-...`).
    pub public_id: String,
    pub project_id: String,
    pub description: String,
    pub scopes: Vec<String>,
    /// When set, only these tool/capability ids may be called with this key.
    pub tool_allow: Option<Vec<String>>,
    pub per_key_daily_cap_micros: Option<i64>,
    /// Extra projects this key may address with `X-Project`.
    pub extra_projects: Vec<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgKey {
    pub org_id: String,
    pub dek_wrapped: Vec<u8>,
    pub kek_version: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialRow {
    pub id: String,
    pub org_id: String,
    pub provider_id: String,
    pub name: String,
    /// api_key | oauth | basic | header_set
    pub kind: String,
    #[serde(skip_serializing)]
    pub ciphertext: Vec<u8>,
    #[serde(skip_serializing)]
    pub nonce: Vec<u8>,
    /// Non-secret metadata (Scribase org/project, OAuth expiry).
    pub meta: Value,
    pub created_at: DateTime<Utc>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleRow {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub description: String,
    pub skill_md: String,
    pub tool_ids: Vec<String>,
    pub credential_refs: Vec<String>,
    pub version: i32,
    pub updated_at: DateTime<Utc>,
}

/// A team-registered private API (admin-only, https only) for passthrough.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ByoTool {
    pub id: String,
    pub org_id: String,
    pub base_url: String,
    pub credential_id: Option<String>,
    /// Header to place the credential in, and an optional prefix.
    pub header: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
}

/// A call to be appended (hashes are computed by the store).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewCall {
    pub id: String,
    pub project_id: String,
    pub org_id: String,
    pub key_public_id: String,
    pub target: String,
    pub tool_id: Option<String>,
    pub provider_id: Option<String>,
    pub side_effect: String,
    pub idempotency_key: Option<String>,
    pub request_hash: String,
    /// ok | failed | rejected
    pub status: String,
    pub http_status: i32,
    pub latency_ms: i64,
    pub cost_micros: i64,
    pub price_basis: String,
    pub mint_hold_id: Option<String>,
    pub mint_receipt: Option<String>,
    pub rago_event_id: Option<String>,
    /// miss | hit | dedupe | replay | none
    pub cache: String,
    pub fallback_chain: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRow {
    #[serde(flatten)]
    pub call: NewCall,
    /// Per-project sequence (1-based).
    pub seq: i64,
    pub prev_hash: String,
    pub row_hash: String,
}

impl NewCall {
    /// The canonical JSON the audit hash covers.
    pub fn hash_payload(&self, seq: i64) -> Value {
        serde_json::json!({
            "seq": seq,
            "id": self.id,
            "project_id": self.project_id,
            "key": self.key_public_id,
            "target": self.target,
            "tool_id": self.tool_id,
            "provider_id": self.provider_id,
            "request_hash": self.request_hash,
            "status": self.status,
            "http_status": self.http_status,
            "cost_micros": self.cost_micros,
            "mint_hold_id": self.mint_hold_id,
            "mint_receipt": self.mint_receipt,
            "cache": self.cache,
            "created_at": self.created_at.timestamp_micros(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdempotencyRow {
    pub project_id: String,
    pub key: String,
    pub request_hash: String,
    pub http_status: i32,
    pub response: Value,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: String,
    /// `rago_event`
    pub kind: String,
    pub payload: Value,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl OutboxItem {
    pub fn new(id: String, kind: &str, payload: Value, now: DateTime<Utc>) -> Self {
        OutboxItem { id, kind: kind.into(), payload, attempts: 0, next_attempt_at: now, delivered_at: None, last_error: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpendFilter {
    pub project_id: String,
    pub since: Option<DateTime<Utc>>,
    pub key_public_id: Option<String>,
    pub target: Option<String>,
    pub side_effect: Option<String>,
    /// Count calls instead of summing cost.
    pub count_calls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Tool,
    Provider,
    Key,
    Day,
}

impl GroupBy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool" => Some(GroupBy::Tool),
            "provider" => Some(GroupBy::Provider),
            "key" => Some(GroupBy::Key),
            "day" => Some(GroupBy::Day),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    pub group: String,
    pub calls: i64,
    pub billed_calls: i64,
    pub cost_micros: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStat {
    pub tool_id: String,
    pub calls: i64,
    pub successes: i64,
    pub total_latency_ms: i64,
    pub observed_cost_micros: i64,
}

impl ToolStat {
    pub fn success_rate(&self) -> f64 {
        if self.calls == 0 {
            1.0
        } else {
            self.successes as f64 / self.calls as f64
        }
    }
    pub fn avg_latency_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_latency_ms as f64 / self.calls as f64
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    /// dcr | cimd | first_party
    pub kind: String,
    pub metadata: Value,
    pub redirect_uris: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCode {
    pub code_hash: String,
    pub client_id: String,
    pub user_id: String,
    pub org_id: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub resource: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthGrant {
    pub id: String,
    pub family_id: String,
    pub client_id: String,
    pub user_id: String,
    pub org_id: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub refresh_hash: String,
    pub resource: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_hash: String,
    pub user_code: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// pending | approved | denied | used
    pub status: String,
    pub user_id: Option<String>,
    pub org_id: Option<String>,
    pub project_id: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub interval_s: i32,
}

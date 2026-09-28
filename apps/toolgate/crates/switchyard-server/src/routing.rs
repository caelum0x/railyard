//! Candidate chains: which tools may serve a target, in which order.
//!
//! Under `free_first` (default) the tiers are: our own products, free/public
//! routes, BYO keys, platform paid keys. Within a tier: observed success rate,
//! then latency. A candidate whose adapter does not accept the caller's actual
//! input, lacks a credential, or lacks a scope is skipped, and the reason is
//! disclosed.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use switchyard_core::{Acceptance, Adapter, ApiError, Credential, ProviderKind, SideEffect, ToolManifest};

use crate::principal::Principal;
use crate::state::AppState;
use crate::vaultops;

pub struct Candidate {
    pub tool: ToolManifest,
    pub kind: ProviderKind,
    pub adapter: Arc<dyn Adapter>,
    pub credential: Option<Credential>,
    pub metered: bool,
    pub quote: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub tool: String,
    pub provider: String,
    pub reason: String,
}

pub struct Plan {
    pub target: String,
    pub capability: String,
    pub is_capability: bool,
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
    /// Fallback allowed (capability target, no pin, policy not `pinned`).
    pub fallback: bool,
}

impl Plan {
    pub fn side_effect(&self) -> SideEffect {
        self.candidates.first().map(|c| c.tool.side_effect).unwrap_or(SideEffect::Read)
    }
    pub fn hold_amount(&self) -> i64 {
        if self.fallback {
            self.candidates.iter().map(|c| c.quote).max().unwrap_or(0)
        } else {
            self.candidates.first().map(|c| c.quote).unwrap_or(0)
        }
    }
}

pub async fn plan(st: &AppState, p: &Principal, target: &str, input: &Value, pin: Option<&str>, stats: &HashMap<String, (f64, f64)>) -> Result<Plan, ApiError> {
    let (tools, capability, is_capability): (Vec<ToolManifest>, String, bool) = if let Some(t) = st.catalog.tool(target) {
        (vec![t.clone()], t.capability.clone(), false)
    } else if let Some(c) = st.catalog.capability(target) {
        (st.catalog.tools_for_capability(&c.id).into_iter().cloned().collect(), c.id.clone(), true)
    } else {
        return Err(ApiError::not_found(format!("no tool or capability named {target}; try GET /v1/tools/search?q=...")));
    };
    let mut skipped = Vec::new();
    let mut candidates = Vec::new();
    for t in tools {
        let skip = |reason: String| Skipped { tool: t.id.clone(), provider: t.provider.clone(), reason };
        if let Some(pin) = pin {
            if t.provider != pin && t.id != pin {
                continue;
            }
        }
        if !p.tool_allowed(&[target, &t.id, &t.capability]) {
            skipped.push(skip("not in this key's tool allowlist".into()));
            continue;
        }
        if !switchyard_auth::scopes::has(&p.scopes, t.side_effect.required_scope()) {
            skipped.push(skip(format!("needs scope {}", t.side_effect.required_scope())));
            continue;
        }
        let Some(adapter) = st.adapters.get(&t.id).cloned() else {
            skipped.push(skip("no adapter".into()));
            continue;
        };
        if let Acceptance::No(why) = adapter.accepts(&t.id, input) {
            skipped.push(skip(why));
            continue;
        }
        if st.flow.is_open(&t.provider) {
            skipped.push(skip("circuit open after repeated failures; retry in a minute".into()));
            continue;
        }
        let kind = st.catalog.provider(&t.provider).map(|p| p.kind).unwrap_or(ProviderKind::Public);
        if kind == ProviderKind::Platform && !st.catalog.provider(&t.provider).map(|p| p.tos_resale_ok).unwrap_or(false) {
            skipped.push(skip("provider terms do not allow resale".into()));
            continue;
        }
        let credential = match vaultops::resolve(st, &p.org.id, &t).await {
            Ok(c) => c,
            Err(reason) => {
                skipped.push(skip(reason));
                continue;
            }
        };
        let metered = vaultops::metered(kind, credential.as_ref());
        let quote = if metered { adapter.quote(&t.id, input) } else { 0 };
        candidates.push(Candidate { tool: t, kind, adapter, credential, metered, quote });
    }
    if let Some(pin) = pin {
        if candidates.is_empty() && skipped.is_empty() {
            return Err(ApiError::bad_request(format!("provider {pin} does not serve {target}")));
        }
    }
    let policy = p.project.routing_policy.as_str();
    let rate = |id: &str| stats.get(id).map(|s| s.0).unwrap_or(1.0);
    let lat = |id: &str| stats.get(id).map(|s| s.1).unwrap_or(0.0);
    candidates.sort_by(|a, b| {
        let key_free = |c: &Candidate| (c.kind.tier(), (1000.0 - rate(&c.tool.id) * 1000.0) as i64, lat(&c.tool.id) as i64);
        match policy {
            "cheapest" => (a.quote, key_free(a).1).cmp(&(b.quote, key_free(b).1)),
            "fastest" => (lat(&a.tool.id) as i64, a.kind.tier()).cmp(&(lat(&b.tool.id) as i64, b.kind.tier())),
            _ => key_free(a).cmp(&key_free(b)),
        }
    });
    let fallback = is_capability && pin.is_none() && policy != "pinned";
    if !fallback {
        candidates.truncate(1);
    }
    Ok(Plan { target: target.into(), capability, is_capability, candidates, skipped, fallback })
}

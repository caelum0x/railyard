//! API-key auth middleware. Every route except the public probes, webhooks,
//! checkout return pages and the console shell requires an `X-API-Key`.
//!
//! Two kinds of key are accepted, both compared in constant time:
//! - the master `MINT_API_KEY` — full authority (operators, migrations);
//! - named, SCOPED keys from `MINT_API_KEYS_JSON` — one per consuming service
//!   (AutoGTM, Ledgerline, Abacus …) holding only the scopes it needs, so a
//!   leaked service key cannot, say, redeem or mint. Multiple keys may share a
//!   name, which is how a key is rotated without downtime (add new → deploy
//!   callers → remove old).

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use mint_store::LedgerStore;
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;

/// Header carrying the API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// A coarse capability a scoped key may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every `GET` under `/v1` (balances, receipts, statements, reserve …).
    Read,
    /// Create accounts / addresses and start KYC sessions.
    Accounts,
    /// Move money between existing accounts: transfers, holds, streams,
    /// allowances and the AITP-402 protocol.
    Spend,
    /// Fund accounts: hosted checkouts and deposit-gated mint.
    Funding,
    /// Cash out: redeem (burn + payout) and payout retries.
    Payouts,
    /// Everything (equivalent to the master key).
    Admin,
}

/// One named scoped key.
#[derive(Clone, Debug, Deserialize)]
pub struct ScopedKey {
    /// Operator-facing label (logged on use; never the key itself).
    pub name: String,
    /// The secret presented in `X-API-Key`.
    pub key: String,
    /// Granted capabilities.
    pub scopes: Vec<Scope>,
}

impl ScopedKey {
    fn allows(&self, needed: Scope) -> bool {
        self.scopes.iter().any(|s| *s == Scope::Admin || *s == needed)
    }
}

/// Parse `MINT_API_KEYS_JSON` (`[{"name","key","scopes":[...]}]`).
pub fn parse_scoped_keys(raw: &str) -> Result<Vec<ScopedKey>, String> {
    let keys: Vec<ScopedKey> = serde_json::from_str(raw)
        .map_err(|e| format!("MINT_API_KEYS_JSON must be a JSON array of keys: {e}"))?;
    for k in &keys {
        if k.name.trim().is_empty() {
            return Err("MINT_API_KEYS_JSON: every key needs a non-empty name".into());
        }
        if k.key.len() < 24 {
            return Err(format!(
                "MINT_API_KEYS_JSON: key '{}' is too short (min 24 chars)",
                k.name
            ));
        }
        if k.scopes.is_empty() {
            return Err(format!("MINT_API_KEYS_JSON: key '{}' has no scopes", k.name));
        }
    }
    Ok(keys)
}

/// The scope a request needs, from its method + path.
pub fn required_scope(method: &Method, path: &str) -> Scope {
    if *method == Method::GET {
        return Scope::Read;
    }
    let p = path.trim_end_matches('/');
    if p == "/v1/accounts"
        || (p.starts_with("/v1/accounts/") && (p.ends_with("/addresses") || p.ends_with("/kyc/session")))
    {
        return Scope::Accounts;
    }
    if p == "/v1/checkouts" || p == "/v1/mint" {
        return Scope::Funding;
    }
    if p == "/v1/redeem" || p.starts_with("/v1/payouts/") {
        return Scope::Payouts;
    }
    if p.starts_with("/v1/transfers")
        || p.starts_with("/v1/holds")
        || p.starts_with("/v1/streams")
        || p.starts_with("/v1/allowances")
        || p.starts_with("/v1/aitp/")
    {
        return Scope::Spend;
    }
    // Anything unclassified is operator-only.
    Scope::Admin
}

fn ct_eq(expected: &[u8], provided: &[u8]) -> bool {
    // subtle's slice `ct_eq` returns 0 for unequal lengths, then compares bytes
    // in constant time — no early-exit timing leak on the shared prefix.
    !expected.is_empty() && bool::from(expected.ct_eq(provided))
}

/// Reject the request with `401` unless `X-API-Key` matches the master key or a
/// scoped key (constant time), and with `403` when a scoped key lacks the
/// route's scope.
pub async fn require_api_key<S: LedgerStore + 'static>(
    State(state): State<AppState<S>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    if ct_eq(state.api_key.as_bytes(), &provided) {
        return Ok(next.run(req).await);
    }
    // Check every scoped key (no early exit) so timing does not reveal which
    // key prefix matched.
    let matched = state
        .scoped_keys
        .iter()
        .fold(None, |acc: Option<&ScopedKey>, k| {
            if ct_eq(k.key.as_bytes(), &provided) {
                Some(k)
            } else {
                acc
            }
        });
    let Some(key) = matched else {
        return Err(ApiError::Unauthorized);
    };
    let needed = required_scope(req.method(), req.uri().path());
    if !key.allows(needed) {
        tracing::warn!(key = %key.name, ?needed, path = %req.uri().path(), "scoped key denied");
        return Err(ApiError::Forbidden(format!(
            "API key '{}' lacks the '{}' scope",
            key.name,
            format!("{needed:?}").to_lowercase()
        )));
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_classify_routes() {
        assert_eq!(required_scope(&Method::GET, "/v1/reserve"), Scope::Read);
        assert_eq!(required_scope(&Method::POST, "/v1/transfers"), Scope::Spend);
        assert_eq!(required_scope(&Method::POST, "/v1/holds/h1/capture"), Scope::Spend);
        assert_eq!(required_scope(&Method::POST, "/v1/redeem"), Scope::Payouts);
        assert_eq!(required_scope(&Method::POST, "/v1/payouts/stripe:x/retry"), Scope::Payouts);
        assert_eq!(required_scope(&Method::POST, "/v1/mint"), Scope::Funding);
        assert_eq!(required_scope(&Method::POST, "/v1/accounts"), Scope::Accounts);
        assert_eq!(required_scope(&Method::POST, "/v1/accounts/a/kyc/session"), Scope::Accounts);
    }

    #[test]
    fn scoped_key_parsing_rejects_weak_keys() {
        assert!(parse_scoped_keys(r#"[{"name":"x","key":"short","scopes":["read"]}]"#).is_err());
        assert!(parse_scoped_keys(r#"[{"name":"x","key":"aaaaaaaaaaaaaaaaaaaaaaaaaa","scopes":[]}]"#).is_err());
        let ok = parse_scoped_keys(
            r#"[{"name":"ledgerline","key":"aaaaaaaaaaaaaaaaaaaaaaaaaa","scopes":["read","spend"]}]"#,
        )
        .unwrap();
        assert!(ok[0].allows(Scope::Spend));
        assert!(!ok[0].allows(Scope::Payouts));
    }
}

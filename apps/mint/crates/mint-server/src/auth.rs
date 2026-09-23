//! API-key auth middleware. Every route except the public probes, webhooks,
//! checkout return pages and the console shell requires an `X-API-Key`.
//!
//! Two kinds of key are accepted, both compared in constant time:
//! - the master `MINT_API_KEY` — full authority (operators, migrations);
//! - DB-backed keys created from the console (`POST /v1/keys`), stored as a
//!   SHA-256 digest only and revocable at runtime;
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
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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

impl Scope {
    /// Parse a stable scope name (`read`, `accounts`, `spend`, `funding`,
    /// `payouts`, `admin`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "read" => Some(Scope::Read),
            "accounts" => Some(Scope::Accounts),
            "spend" => Some(Scope::Spend),
            "funding" => Some(Scope::Funding),
            "payouts" => Some(Scope::Payouts),
            "admin" => Some(Scope::Admin),
            _ => None,
        }
    }

    /// The stable lowercase name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Accounts => "accounts",
            Scope::Spend => "spend",
            Scope::Funding => "funding",
            Scope::Payouts => "payouts",
            Scope::Admin => "admin",
        }
    }
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
    /// Optional account allow-list: when set, the key may only move money
    /// out of, fund, and read these account ids.
    #[serde(default)]
    pub accounts: Option<Vec<String>>,
}

impl ScopedKey {
    pub fn allows(&self, needed: Scope) -> bool {
        scopes_allow(&self.scopes, needed)
    }
}

fn scopes_allow(scopes: &[Scope], needed: Scope) -> bool {
    scopes.iter().any(|s| *s == Scope::Admin || *s == needed)
}

/// Parse `MINT_API_KEYS_JSON` (`[{"name","key","scopes":[...],"accounts":[...]?}]`).
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
        if let Some(accts) = &k.accounts {
            if accts.is_empty() {
                return Err(format!(
                    "MINT_API_KEYS_JSON: key '{}' has an empty account list (omit it for all accounts)",
                    k.name
                ));
            }
            for a in accts {
                if mint_core::AccountId::parse(a).is_err() {
                    return Err(format!(
                        "MINT_API_KEYS_JSON: key '{}' names an invalid account id '{a}'",
                        k.name
                    ));
                }
            }
        }
    }
    Ok(keys)
}

/// The scope a request needs, from its method + path.
pub fn required_scope(method: &Method, path: &str) -> Scope {
    let p = path.trim_end_matches('/');
    // Key management is operator-only, even for reads.
    if p == "/v1/keys" || p.starts_with("/v1/keys/") {
        return Scope::Admin;
    }
    // AITP payer-key registration authorizes spend: operator-only too.
    if p == "/v1/payer-keys" || p.starts_with("/v1/payer-keys/") {
        return Scope::Admin;
    }
    if *method == Method::GET {
        return Scope::Read;
    }
    if p == "/v1/accounts"
        || (p.starts_with("/v1/accounts/") && (p.ends_with("/addresses") || p.ends_with("/kyc/session")))
    {
        return Scope::Accounts;
    }
    if p == "/v1/checkouts" || p == "/v1/mint" {
        return Scope::Funding;
    }
    if p == "/v1/redeem" || (p.starts_with("/v1/payouts/") && p.ends_with("/retry")) {
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
    // Anything unclassified (freeze / close, payout cancel / mark-paid,
    // reconciliation imports, reversals …) is operator-only.
    Scope::Admin
}

fn ct_eq(expected: &[u8], provided: &[u8]) -> bool {
    // subtle's slice `ct_eq` returns 0 for unequal lengths, then compares bytes
    // in constant time — no early-exit timing leak on the shared prefix.
    !expected.is_empty() && bool::from(expected.ct_eq(provided))
}

/// Lowercase hex SHA-256 of a presented secret (the DB-backed key lookup key).
pub fn hash_key(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// The authenticated caller, attached to the request extensions so handlers
/// can apply the key's account restriction.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// Key label (`master` for `MINT_API_KEY`).
    pub key_name: String,
    /// `None` ⇒ every account; `Some` ⇒ only these account ids.
    pub accounts: Option<HashSet<String>>,
}

impl AuthContext {
    /// `true` when the key may act on `account_id`.
    pub fn may_access(&self, account_id: &str) -> bool {
        self.accounts.as_ref().is_none_or(|set| set.contains(account_id))
    }

    /// `true` when the key carries an account allow-list.
    pub fn is_restricted(&self) -> bool {
        self.accounts.is_some()
    }

    /// `Forbidden` unless the key may act on `account_id`.
    pub fn require(&self, account_id: &str) -> Result<(), ApiError> {
        if self.may_access(account_id) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(format!(
                "API key '{}' is not permitted to act on account {account_id}",
                self.key_name
            )))
        }
    }
}

/// Maximum body buffered when enforcing an account restriction.
const MAX_INSPECTED_BODY: usize = 1024 * 1024;

/// Reject the request with `401` unless `X-API-Key` matches the master key, an
/// env scoped key (constant time) or an active DB-backed key (hash lookup);
/// with `403` when the key lacks the route's scope; and with `403` when an
/// account-restricted key touches an account outside its allow-list.
pub async fn require_api_key<S: LedgerStore + 'static>(
    State(state): State<AppState<S>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if ct_eq(state.api_key.as_bytes(), provided.as_bytes()) {
        req.extensions_mut().insert(AuthContext {
            key_name: "master".into(),
            accounts: None,
        });
        return Ok(next.run(req).await);
    }
    // Check every env scoped key (no early exit) so timing does not reveal
    // which key prefix matched.
    let matched = state
        .scoped_keys
        .iter()
        .fold(None, |acc: Option<&ScopedKey>, k| {
            if ct_eq(k.key.as_bytes(), provided.as_bytes()) {
                Some(k)
            } else {
                acc
            }
        });
    let (name, scopes, accounts) = match matched {
        Some(k) => (k.name.clone(), k.scopes.clone(), k.accounts.clone()),
        None => {
            if provided.len() < 24 {
                return Err(ApiError::Unauthorized);
            }
            // DB-backed keys are looked up by SHA-256 (the digest, not the
            // secret, is the index key, so the lookup leaks nothing useful).
            let rec = state
                .ledger
                .store()
                .find_api_key(&hash_key(&provided))
                .await
                .map_err(|_| ApiError::Unavailable("API key store unavailable".into()))?
                .ok_or(ApiError::Unauthorized)?;
            let scopes: Vec<Scope> = rec.scopes.iter().filter_map(|s| Scope::parse(s)).collect();
            (rec.name, scopes, rec.accounts)
        }
    };
    let needed = required_scope(req.method(), req.uri().path());
    if !scopes_allow(&scopes, needed) {
        tracing::warn!(key = %name, ?needed, path = %req.uri().path(), "scoped key denied");
        return Err(ApiError::Forbidden(format!(
            "API key '{name}' lacks the '{}' scope",
            needed.as_str()
        )));
    }
    let ctx = AuthContext {
        key_name: name,
        accounts: accounts.map(|v| v.into_iter().collect()),
    };
    let req = if ctx.is_restricted() {
        enforce_account_restriction(&state, &ctx, needed, req).await?
    } else {
        req
    };
    let mut req = req;
    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}

/// Resolve every account a request acts on and require each to be in the
/// key's allow-list. Global views (all accounts, the audit log, operator
/// reconciliation) are refused outright for restricted keys.
async fn enforce_account_restriction<S: LedgerStore + 'static>(
    state: &AppState<S>,
    ctx: &AuthContext,
    needed: Scope,
    req: Request,
) -> Result<Request, ApiError> {
    let deny = |what: &str| {
        Err(ApiError::Forbidden(format!(
            "API key '{}' is restricted to specific accounts and cannot {what}",
            ctx.key_name
        )))
    };
    if needed == Scope::Admin {
        return deny("use operator routes");
    }
    let method = req.method().clone();
    let path = req.uri().path().trim_end_matches('/').to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let store = state.ledger.store();

    // ---- path-addressed resources ----
    match segs.as_slice() {
        ["v1", "accounts"] if method == Method::GET => return deny("list every account"),
        ["v1", "accounts", id, ..] => ctx.require(id)?,
        ["v1", "addresses", address] => {
            let addr = mint_core::Address::parse(address).map_err(ApiError::from)?;
            let owner = store.lookup_address(&addr).await?;
            ctx.require(owner.as_str())?;
        }
        ["v1", "holds", id, ..] => {
            let hold_id = mint_core::HoldId::parse(id).map_err(ApiError::from)?;
            let hold = store.get_hold(&hold_id).await?;
            ctx.require(hold.account_id.as_str())?;
        }
        ["v1", "streams", id, ..] => {
            let sid = mint_core::StreamId::parse(id).map_err(ApiError::from)?;
            let stream = store.get_stream(&sid).await?;
            if !(ctx.may_access(stream.from_id.as_str())
                || (method == Method::GET && ctx.may_access(stream.to_id.as_str())))
            {
                ctx.require(stream.from_id.as_str())?;
            }
        }
        ["v1", "allowances", id, ..] => {
            let aid = mint_core::AllowanceId::parse(id).map_err(ApiError::from)?;
            let alw = store.get_allowance(&aid).await?;
            if !ctx.may_access(alw.granter.as_str()) {
                // A delegate may draw on an allowance only toward its own
                // accounts; checked against the body `to` below.
                if !(segs.last() == Some(&"spend")) {
                    ctx.require(alw.granter.as_str())?;
                }
            }
        }
        ["v1", "payouts"] => {
            let account = query
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "account_id")
                .map(|(_, v)| v.to_string());
            match account {
                Some(a) => ctx.require(&a)?,
                None => return deny("list payouts without an account_id filter"),
            }
        }
        ["v1", "payouts", payout_ref, ..] => {
            let rec = store
                .get_payout(payout_ref)
                .await?
                .ok_or_else(|| ApiError::NotFound(format!("no payout {payout_ref}")))?;
            ctx.require(rec.account_id.as_str())?;
        }
        ["v1", "receipts", ..] => return deny("read the global receipt log"),
        ["v1", "reconciliation", ..] => return deny("read reconciliation"),
        ["v1", "reversals", ..] => return deny("read reversals"),
        _ => {}
    }

    if method != Method::POST {
        return Ok(req);
    }

    // ---- body-addressed accounts (the SOURCE of funds / funded account) ----
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_INSPECTED_BODY)
        .await
        .map_err(|_| ApiError::BadRequest("request body too large or unreadable".into()))?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    let field = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let source_fields: &[&str] = match segs.as_slice() {
        ["v1", "transfers"] | ["v1", "holds"] | ["v1", "streams"] | ["v1", "redeem"] => &["from"],
        ["v1", "allowances"] => &["granter"],
        ["v1", "mint"] => &["to"],
        ["v1", "checkouts"] => &["account_id"],
        ["v1", "aitp", "verify"] => &["payer"],
        _ => &[],
    };
    for f in source_fields {
        match field(f) {
            Some(acct) => ctx.require(&acct)?,
            None => {
                return Err(ApiError::BadRequest(format!("missing '{f}'")));
            }
        }
    }
    // Hold-addressed AITP settlement: the hold's owner must be allowed.
    if let ["v1", "aitp", "capture" | "release"] = segs.as_slice() {
        let hold_id = field("hold_id").ok_or_else(|| ApiError::BadRequest("missing 'hold_id'".into()))?;
        let hold_id = mint_core::HoldId::parse(&hold_id).map_err(ApiError::from)?;
        let hold = store.get_hold(&hold_id).await?;
        ctx.require(hold.account_id.as_str())?;
    }
    // Delegated allowance spend: either the granter (checked above) or the
    // payee must be one of the key's accounts.
    if let ["v1", "allowances", id, "spend"] = segs.as_slice() {
        let aid = mint_core::AllowanceId::parse(id).map_err(ApiError::from)?;
        let alw = store.get_allowance(&aid).await?;
        let to = field("to").unwrap_or_default();
        if !ctx.may_access(alw.granter.as_str()) {
            ctx.require(&to)?;
        }
    }
    Ok(Request::from_parts(parts, axum::body::Body::from(bytes)))
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
        assert_eq!(required_scope(&Method::POST, "/v1/accounts/a/freeze"), Scope::Admin);
        assert_eq!(required_scope(&Method::POST, "/v1/payouts/stripe:x/cancel"), Scope::Admin);
        assert_eq!(required_scope(&Method::GET, "/v1/keys"), Scope::Admin);
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

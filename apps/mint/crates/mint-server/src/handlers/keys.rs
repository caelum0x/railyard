//! DB-backed API key management (operator-only): list, create (the secret is
//! returned exactly once) and revoke. Keys may be restricted to specific
//! account ids, enforced by the auth middleware on every request.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_store::{ApiKeyRecord, LedgerStore};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{hash_key, Scope};
use crate::dto::parse_account;
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;

/// Secret prefix so leaked keys are recognisable by secret scanners.
const SECRET_PREFIX: &str = "mint_sk_";
/// Random bytes in a generated secret (hex-encoded ⇒ 64 chars).
const SECRET_BYTES: usize = 32;
/// Characters of the secret kept for display.
const DISPLAY_PREFIX_LEN: usize = 14;
/// Upper bound on an account allow-list.
const MAX_KEY_ACCOUNTS: usize = 500;

/// `POST /v1/keys` body.
#[derive(Debug, Deserialize)]
pub struct CreateKeyReq {
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub accounts: Option<Vec<String>>,
}

fn key_view(k: &ApiKeyRecord) -> Value {
    json!({
        "id": k.id,
        "name": k.name,
        "prefix": k.prefix,
        "scopes": k.scopes,
        "accounts": k.accounts,
        "created_ms": k.created_ms,
        "revoked_ms": k.revoked_ms,
        "active": k.revoked_ms.is_none(),
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `GET /v1/keys` → every DB-backed key (never the secret), plus the names of
/// the env-configured keys so operators see the full picture.
pub async fn list<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let keys = st.ledger.store().list_api_keys().await?;
    let env_keys: Vec<Value> = st
        .scoped_keys
        .iter()
        .map(|k| {
            json!({
                "name": k.name,
                "scopes": k.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "accounts": k.accounts,
            })
        })
        .collect();
    Ok(ok(json!({
        "items": keys.iter().map(key_view).collect::<Vec<_>>(),
        "env_keys": env_keys,
    })))
}

/// `POST /v1/keys` → mint a new key. The response carries the plaintext
/// `secret` ONCE; only its SHA-256 is stored.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<CreateKeyReq>,
) -> ApiResult {
    let name = body.name.trim().to_string();
    if name.is_empty() || name.len() > 80 {
        return Err(ApiError::BadRequest("name must be 1-80 characters".into()));
    }
    if body.scopes.is_empty() {
        return Err(ApiError::BadRequest("select at least one scope".into()));
    }
    let mut scopes: Vec<String> = Vec::new();
    for raw in &body.scopes {
        let scope = Scope::parse(raw.trim()).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown scope '{raw}' (expected read, accounts, spend, funding, payouts, admin)"
            ))
        })?;
        if !scopes.iter().any(|s| s == scope.as_str()) {
            scopes.push(scope.as_str().to_string());
        }
    }
    let accounts = match body.accounts {
        None => None,
        Some(list) => {
            let list: Vec<String> = list
                .into_iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect();
            if list.is_empty() {
                None
            } else {
                if list.len() > MAX_KEY_ACCOUNTS {
                    return Err(ApiError::BadRequest(format!(
                        "at most {MAX_KEY_ACCOUNTS} accounts per key"
                    )));
                }
                if scopes.iter().any(|s| s == "admin") {
                    return Err(ApiError::BadRequest(
                        "an account-restricted key cannot hold the admin scope".into(),
                    ));
                }
                let mut out = Vec::with_capacity(list.len());
                for raw in list {
                    let id = parse_account(&raw).map_err(ApiError::from)?;
                    // Existence check: a typo must not silently lock a key out.
                    st.ledger.store().get_account(&id).await?;
                    if !out.contains(&raw) {
                        out.push(raw);
                    }
                }
                Some(out)
            }
        }
    };

    let mut bytes = [0u8; SECRET_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let secret = format!("{SECRET_PREFIX}{}", hex::encode(bytes));
    let record = ApiKeyRecord {
        id: format!("key_{}", ulid::Ulid::new()),
        name,
        key_hash: hash_key(&secret),
        prefix: secret.chars().take(DISPLAY_PREFIX_LEN).collect(),
        scopes,
        accounts,
        created_ms: now_ms(),
        revoked_ms: None,
    };
    let stored = st.ledger.store().insert_api_key(record).await?;
    tracing::info!(key = %stored.name, id = %stored.id, "API key created");
    let mut view = key_view(&stored);
    view["secret"] = Value::String(secret);
    Ok(ok_status(StatusCode::CREATED, view))
}

/// `POST /v1/keys/:id/revoke` → the key stops authenticating immediately
/// (idempotent).
pub async fn revoke<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let rec = st
        .ledger
        .store()
        .revoke_api_key(&id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no API key {id}")))?;
    tracing::info!(key = %rec.name, id = %rec.id, "API key revoked");
    Ok(ok(key_view(&rec)))
}

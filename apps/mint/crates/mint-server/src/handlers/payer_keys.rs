//! AITP-402 payer verifying keys (operator-only): register, list and revoke
//! the Ed25519 public keys agents sign `AITP-Authorization`s with. Changes
//! take effect immediately on this instance (the in-process mirror is
//! reloaded after every mutation) and on every other instance within one
//! background sweep.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::MintError;
use mint_store::{LedgerStore, PayerKeyRecord};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::parse_account;
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::jobs::now_ms;
use crate::payer_registry::parse_public_key_hex;
use crate::state::AppState;

/// Longest accepted `pubkey_id` / label.
const MAX_ID_LEN: usize = 80;

/// `POST /v1/payer-keys` body.
#[derive(Debug, Deserialize)]
pub struct CreatePayerKeyReq {
    /// The paying account (`acct_…`).
    pub payer: String,
    /// Key id the agent echoes in `Authorization.pubkey_id`.
    pub pubkey_id: String,
    /// Hex of the 32-byte Ed25519 public key.
    pub public_key_hex: String,
    /// Optional operator label.
    #[serde(default)]
    pub label: Option<String>,
}

/// `GET /v1/payer-keys` query.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Only this payer's keys.
    pub payer: Option<String>,
}

fn view(k: &PayerKeyRecord) -> Value {
    json!({
        "payer": k.payer,
        "pubkey_id": k.pubkey_id,
        "public_key_hex": k.public_key_hex,
        "label": k.label,
        "created_ms": k.created_ms,
        "revoked_ms": k.revoked_ms,
        "active": k.revoked_ms.is_none(),
    })
}

fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

async fn refresh_mirror<S: LedgerStore + 'static>(st: &AppState<S>) {
    if let Err(e) = st.payer_keys.reload(st.ledger.store()).await {
        tracing::error!(error = %e, "payer-key mirror reload failed after a mutation");
    }
}

/// `GET /v1/payer-keys` → every registered key (active and revoked), newest
/// first, optionally for one payer.
pub async fn list<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Query(p): Query<ListParams>,
) -> ApiResult {
    let payer = match p.payer.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_account(raw).map_err(ApiError::from)?.as_str().to_string()),
        None => None,
    };
    let keys = st.ledger.store().list_payer_keys(payer.as_deref()).await?;
    Ok(ok(json!({
        "items": keys.iter().map(view).collect::<Vec<_>>(),
        "active_count": keys.iter().filter(|k| k.revoked_ms.is_none()).count(),
    })))
}

/// `POST /v1/payer-keys` → register a payer verifying key.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<CreatePayerKeyReq>,
) -> ApiResult {
    let payer = parse_account(body.payer.trim()).map_err(ApiError::from)?;
    // Existence check first so a typo is a clear 404, not a dangling key.
    st.ledger.store().get_account(&payer).await?;
    let pubkey_id = body.pubkey_id.trim().to_string();
    if !valid_key_id(&pubkey_id) {
        return Err(ApiError::BadRequest(format!(
            "pubkey_id must be 1-{MAX_ID_LEN} characters of letters, digits, '-', '_', '.', ':'"
        )));
    }
    let public_key_hex = body.public_key_hex.trim().to_ascii_lowercase();
    parse_public_key_hex(&public_key_hex).map_err(ApiError::BadRequest)?;
    let label = body
        .label
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());
    if label.as_ref().map_or(false, |l| l.len() > MAX_ID_LEN) {
        return Err(ApiError::BadRequest(format!(
            "label must be at most {MAX_ID_LEN} characters"
        )));
    }
    let record = PayerKeyRecord {
        payer: payer.as_str().to_string(),
        pubkey_id,
        public_key_hex,
        label,
        created_ms: now_ms(),
        revoked_ms: None,
    };
    let saved = match st.ledger.store().insert_payer_key(record).await {
        Ok(k) => k,
        Err(MintError::IdempotencyConflict) => {
            return Err(ApiError::BadRequest(
                "an active key with this pubkey_id already exists for the payer; revoke it first"
                    .into(),
            ))
        }
        Err(e) => return Err(e.into()),
    };
    refresh_mirror(&st).await;
    Ok(ok_status(StatusCode::CREATED, view(&saved)))
}

/// `POST /v1/payer-keys/:payer/:pubkey_id/revoke` → revoke (idempotent). A
/// revoked key stops verifying immediately, including one also present in
/// the static `MINT_PAYER_KEYS_JSON` bootstrap set.
pub async fn revoke<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path((payer, pubkey_id)): Path<(String, String)>,
) -> ApiResult {
    let payer = parse_account(payer.trim()).map_err(ApiError::from)?;
    let rec = st
        .ledger
        .store()
        .revoke_payer_key(payer.as_str(), pubkey_id.trim())
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!("no payer key {pubkey_id} for {}", payer.as_str()))
        })?;
    refresh_mirror(&st).await;
    Ok(ok(view(&rec)))
}

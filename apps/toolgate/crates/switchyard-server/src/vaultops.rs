//! Credential storage (write-only) and just-in-time resolution.

use chrono::Utc;
use secrecy::SecretString;
use serde_json::Value;
use switchyard_core::{new_id, ApiError, AuthMode, Credential, CredentialKind, ProviderKind, ToolManifest};
use switchyard_store::{CredentialRow, OrgKey};
use switchyard_vault::{Sealed, WrappedDek};

use crate::state::AppState;

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "vault error");
    ApiError::internal("credential vault error")
}

async fn org_dek(st: &AppState, org_id: &str) -> Result<WrappedDek, ApiError> {
    let existing = st.store.get_org_key(org_id).await.map_err(internal)?;
    let k = match existing {
        Some(k) => k,
        None => {
            let w = st.vault.new_org_dek(org_id).map_err(internal)?;
            st.store
                .ensure_org_key(OrgKey { org_id: org_id.into(), dek_wrapped: w.wrapped, kek_version: w.kek_version as i32, created_at: Utc::now() })
                .await
                .map_err(internal)?
        }
    };
    Ok(WrappedDek { wrapped: k.dek_wrapped, kek_version: k.kek_version as u32 })
}

/// Seal and store a credential; returns the metadata row (never the value).
pub async fn store_credential(st: &AppState, org_id: &str, provider: &str, name: &str, kind: CredentialKind, value: &str, meta: Value) -> Result<CredentialRow, ApiError> {
    if value.trim().is_empty() || value.len() > 16 * 1024 {
        return Err(ApiError::bad_request("credential value must be 1..16384 bytes"));
    }
    let dek = org_dek(st, org_id).await?;
    let id = new_id("cred");
    let sealed = st.vault.seal(&dek, org_id, &id, value).map_err(internal)?;
    st.store
        .insert_credential(CredentialRow {
            id,
            org_id: org_id.into(),
            provider_id: provider.into(),
            name: name.into(),
            kind: kind.as_str().into(),
            ciphertext: sealed.ciphertext,
            nonce: sealed.nonce,
            meta,
            created_at: Utc::now(),
            last_ok_at: None,
            last_error: None,
        })
        .await
        .map_err(internal)
}

async fn open(st: &AppState, row: &CredentialRow) -> Result<Credential, ApiError> {
    let dek = org_dek(st, &row.org_id).await?;
    let secret = st.vault.open(&dek, &row.org_id, &row.id, &Sealed { ciphertext: row.ciphertext.clone(), nonce: row.nonce.clone() }).map_err(internal)?;
    Ok(Credential {
        id: row.id.clone(),
        provider: row.provider_id.clone(),
        kind: CredentialKind::parse(&row.kind).unwrap_or(CredentialKind::ApiKey),
        secret,
        meta: row.meta.clone(),
        platform: false,
    })
}

/// Open a specific stored credential.
pub async fn open_by_id(st: &AppState, org_id: &str, id: &str) -> Result<Credential, ApiError> {
    let row = st.store.get_credential(org_id, id).await.map_err(internal)?.ok_or_else(|| ApiError::not_found("credential not found"))?;
    open(st, &row).await
}

/// The credential ladder for one tool. `Ok(None)` means "no credential
/// needed"; `Err(reason)` means this candidate cannot run for this team.
pub async fn resolve(st: &AppState, org_id: &str, tool: &ToolManifest) -> Result<Option<Credential>, String> {
    let team = match tool.auth {
        AuthMode::None | AuthMode::CallerKey => None,
        _ => match st.store.find_credential(org_id, &tool.provider).await {
            Ok(Some(row)) => {
                let cred = open(st, &row).await.map_err(|e| e.message)?;
                Some(crate::routes::oauth_connect::refresh_if_due(st, org_id, cred).await?)
            }
            Ok(None) => None,
            Err(e) => return Err(format!("credential lookup failed: {e}")),
        },
    };
    match tool.auth {
        AuthMode::None | AuthMode::CallerKey => Ok(None),
        AuthMode::ByoRequired => team.map(Some).ok_or_else(|| format!("needs your own {} credential (sy cred add {})", tool.provider, tool.provider)),
        AuthMode::ByoOptional => Ok(team),
        AuthMode::OwnProduct => match team {
            Some(c) => Ok(Some(c)),
            None => match st.settings.platform_credentials.get(&tool.provider) {
                Some(v) => Ok(Some(Credential {
                    id: format!("platform:{}", tool.provider),
                    provider: tool.provider.clone(),
                    kind: CredentialKind::ApiKey,
                    secret: SecretString::from(v.clone()),
                    meta: Value::Null,
                    platform: true,
                })),
                None => Err(format!("{} is not connected on this Switchyard", tool.provider)),
            },
        },
    }
}

/// Team keys for third-party providers are unmetered; our own products and
/// platform credentials are metered at the tool's price.
pub fn metered(kind: ProviderKind, cred: Option<&Credential>) -> bool {
    !matches!((kind, cred), (ProviderKind::Byo | ProviderKind::Public, Some(c)) if !c.platform)
}

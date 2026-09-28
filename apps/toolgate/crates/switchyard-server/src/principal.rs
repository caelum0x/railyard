//! Who is calling: a Relay `sk-t0-` key bound to a project, or an OAuth
//! access token (MCP clients, CLI device login).

use axum::http::HeaderMap;
use chrono::Utc;
use secrecy::SecretString;
use switchyard_auth::scopes;
use switchyard_core::ApiError;
use switchyard_store::{ApiKey, Org, Project};

use crate::state::AppState;

#[derive(Clone, Debug)]
pub enum Via {
    Key,
    OAuth { family: String, client_id: String, user_id: String },
}

#[derive(Clone)]
pub struct Principal {
    pub via: Via,
    pub key: ApiKey,
    pub org: Org,
    pub project: Project,
    pub scopes: Vec<String>,
    /// The raw Relay key (only for key callers): lets `relay.chat.complete`
    /// bill the same key on the model rail.
    pub caller_key: Option<SecretString>,
}

impl Principal {
    pub fn require(&self, scope: &str) -> Result<(), ApiError> {
        if scopes::has(&self.scopes, scope) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!("this credential lacks the `{scope}` scope")).with_details(serde_json::json!({ "missing_scope": scope })))
        }
    }

    /// Per-key tool allowlist: the target, its capability, or the tool id.
    pub fn tool_allowed(&self, ids: &[&str]) -> bool {
        match &self.key.tool_allow {
            None => true,
            Some(list) => ids.iter().any(|id| list.iter().any(|a| a == id)),
        }
    }
}

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|s| s.trim().to_string())
}

/// Authenticate a request. `audiences` are the resources an OAuth token may be
/// bound to for this surface.
pub async fn authenticate(st: &AppState, headers: &HeaderMap, audiences: &[&str]) -> Result<Principal, ApiError> {
    let token = bearer(headers).ok_or_else(|| ApiError::unauthorized("send Authorization: Bearer <sk-t0 key or OAuth token>"))?;
    if token.starts_with("sk-t0-") {
        let v = st.keys.verify(&token).await?;
        let key = st
            .store
            .get_api_key(&v.public_id)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::forbidden("this Relay key is not enabled for tools; create one with `sy keys create` or POST /v1/keys"))?;
        if key.revoked_at.is_some() {
            return Err(ApiError::unauthorized("this key was revoked for tools"));
        }
        let project_id = match headers.get("x-project").and_then(|v| v.to_str().ok()) {
            Some(p) if p != key.project_id => {
                if !key.extra_projects.iter().any(|x| x == p) {
                    return Err(ApiError::forbidden("this key may not address that project"));
                }
                p.to_string()
            }
            _ => key.project_id.clone(),
        };
        let project = st.store.get_project(&project_id).await.map_err(|_| ApiError::forbidden("project not found"))?;
        let org = st.store.get_org(&project.org_id).await.map_err(|_| ApiError::forbidden("org not found"))?;
        let scopes = key.scopes.clone();
        return Ok(Principal { via: Via::Key, key, org, project, scopes, caller_key: Some(SecretString::from(token)) });
    }
    let claims = st.signer.verify(&token, audiences, Utc::now().timestamp())?;
    if st.store.family_revoked(&claims.fam).await.map_err(|e| ApiError::internal(e.to_string()))? {
        return Err(ApiError::unauthorized("this grant was revoked"));
    }
    // Removing someone from the org cuts their tokens off immediately.
    if st.store.get_member(&claims.org, &claims.sub).await.map_err(|e| ApiError::internal(e.to_string()))?.is_none() {
        return Err(ApiError::unauthorized("you are no longer a member of this org"));
    }
    let project = st.store.get_project(&claims.prj).await.map_err(|_| ApiError::forbidden("project not found"))?;
    if project.org_id != claims.org {
        return Err(ApiError::forbidden("token project does not belong to its org"));
    }
    let org = st.store.get_org(&claims.org).await.map_err(|_| ApiError::forbidden("org not found"))?;
    let scopes = claims.scopes();
    let key = ApiKey {
        public_id: format!("oauth:{}", claims.fam),
        project_id: project.id.clone(),
        description: format!("oauth client {}", claims.cid),
        scopes: scopes.clone(),
        tool_allow: None,
        per_key_daily_cap_micros: None,
        extra_projects: vec![],
        revoked_at: None,
        created_at: Utc::now(),
    };
    Ok(Principal {
        via: Via::OAuth { family: claims.fam.clone(), client_id: claims.cid.clone(), user_id: claims.sub.clone() },
        key,
        org,
        project,
        scopes,
        caller_key: None,
    })
}

/// Audiences accepted on the HTTP API.
pub fn api_audiences(st: &AppState) -> Vec<String> {
    st.oauth.cfg.resources.clone()
}

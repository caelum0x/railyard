//! Public self-serve signup (`POST /v1/auth/signup`, backlog item 6):
//! replaces the admin-token-curl-only onboarding (`/admin/api/orgs` +
//! `/admin/api/projects` + `/admin/api/keys`, all operator-token gated) with
//! one unauthenticated call that creates an org, a default project and a
//! full-scope key for it, so a team can start calling the catalog's free
//! tools immediately and see a 402 with a `topup_url` the moment they touch
//! a paid one -- no operator in the loop for the common case.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use switchyard_core::{new_id, ApiError};
use switchyard_store::Org;

use super::account::{create_key_for, project_view, CreateKey};
use super::admin::new_project;
use crate::http_util::{AppError, ApiResult};
use crate::state::AppState;

const MAX_ORG_NAME: usize = 80;
const SLUG_SUFFIX_LEN: usize = 6;
const MAX_SIGNUP_ATTEMPTS: u8 = 5;

#[derive(Deserialize)]
pub struct SignupReq {
    pub org_name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
}

fn parse(body: &Bytes) -> Result<SignupReq, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

/// `my-cool-team!! Inc.` -> `my-cool-team-inc` (trimmed, lower-cased,
/// collapsed to one dash per run of non `[a-z0-9]`, capped at 24 chars so a
/// random suffix always fits).
fn slugify(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // suppress a leading dash
    for c in raw.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 24 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// A short, URL-safe, lower-case random suffix (no new RNG dependency: reuse
/// the auth crate's token generator and keep only alphanumeric characters).
fn random_suffix() -> String {
    switchyard_auth::random_token()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .take(SLUG_SUFFIX_LEN)
        .collect()
}

fn looks_like_email(s: &str) -> bool {
    matches!(s.split_once('@'), Some((user, domain)) if !user.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.'))
}

pub async fn signup(State(st): State<AppState>, body: Bytes) -> ApiResult {
    if !st.settings.signup_enabled {
        return Err(ApiError::new(404, "signup_disabled", "Self-serve signup is off; ask an operator to create your org.").into());
    }
    let b: SignupReq = parse(&body)?;
    let name = b.org_name.trim();
    if name.is_empty() || name.chars().count() > MAX_ORG_NAME {
        return Err(ApiError::bad_request(format!("org_name must be 1-{MAX_ORG_NAME} characters")).into());
    }
    if let Some(email) = b.email.as_deref() {
        if !looks_like_email(email) {
            return Err(ApiError::bad_request("email must look like name@domain").into());
        }
    }
    let base = slugify(name);
    let base = if base.is_empty() { "org".to_string() } else { base };

    let mut org = Org { id: new_id("org"), slug: format!("{base}-{}", random_suffix()), name: name.to_string(), mint_account_id: None, rago_subscription_id: None, created_at: Utc::now() };
    let mut attempt = 0u8;
    let org = loop {
        match st.store.create_org(org.clone()).await {
            Ok(saved) => break saved,
            Err(switchyard_store::StoreError::Conflict(_)) if attempt + 1 < MAX_SIGNUP_ATTEMPTS => {
                attempt += 1;
                org.slug = format!("{base}-{}", random_suffix());
            }
            Err(e) => return Err(AppError::from(e)),
        }
    };

    let project_name = b.project_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("default");
    let project = st.store.create_project(new_project(&org.id, project_name, None, None)).await?;

    // One full-scope key for the org's own first user: self-serve means no
    // separate operator step to grant admin before the team can manage its
    // own keys, credentials and caps.
    let key_body = CreateKey { description: "signup default key".into(), scopes: vec![switchyard_auth::scopes::ADMIN.to_string()], tool_allow: None, per_key_daily_cap_micros: None };
    let (raw_key, key) = create_key_for(&st, &org.slug, &project.id, key_body).await?;

    tracing::info!(org = %org.slug, project = %project.id, "self-serve signup");
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({
            "ok": true,
            "data": {
                "org": { "id": org.id, "slug": org.slug, "name": org.name },
                "project": project_view(&project),
                "key": raw_key,
                "key_id": key.public_id,
                "note": "Key shown once. It works on both Relay (/openai/v1) and Switchyard (/v1/tools) and already has admin scope for this project -- use it to add credentials, caps and teammates' keys. No Mint billing account yet: free/cached tools work now, paid tools answer 402 with a topup_url once one is attached (ask an operator, or see /catalog).",
                "docs_url": format!("{}/catalog", st.settings.public_url),
            }
        })),
    )
        .into_response())
}

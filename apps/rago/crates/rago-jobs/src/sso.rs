//! SSO login — OAuth2 / OIDC authorization-code flow with Google, Okta or
//! Microsoft Entra ID, then the IdP's userinfo endpoint for the verified email.
//!
//! The flow is server-driven and CSRF-safe:
//!
//! 1. [`start`] mints a single-use random `state` plus a PKCE code verifier,
//!    stores them (10-minute TTL) and returns the IdP authorize URL carrying the
//!    `state` and the `S256` code challenge.
//! 2. The IdP redirects back with `code` + `state`; [`complete`] atomically
//!    consumes the stored `state` (unknown, expired, reused, or issued for a
//!    different provider → rejected), then exchanges the code together with the
//!    bound PKCE verifier and reads the verified email.
//!
//! Env per provider (`GOOGLE`, `OKTA`, `ENTRA_ID`):
//! `RAGO_<P>_CLIENT_ID`, `RAGO_<P>_CLIENT_SECRET`, `RAGO_<P>_REDIRECT_URI`, plus
//! `RAGO_OKTA_DOMAIN` (e.g. `acme.okta.com`) or `RAGO_ENTRA_ID_TENANT_ID`.

use chrono::{Duration, Utc};
use rago_db::Db;
use rago_integrations::oauth::{pkce_challenge, query_string, random_token};
use rago_integrations::{HttpTransport, OAuthClient, OAuthConfig};
use serde_json::Value;

use crate::JobError;

/// How long an issued SSO `state` stays valid.
pub const STATE_TTL_MINUTES: i64 = 10;

struct Idp {
    authorize_url: String,
    userinfo_url: String,
    oauth: OAuthConfig,
}

fn env(name: String) -> Result<String, JobError> {
    std::env::var(&name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| JobError::Integration(format!("idp_not_configured: {name}")))
}

fn idp(provider: &str) -> Result<Idp, JobError> {
    let p = provider.to_uppercase();
    let (authorize_url, token_url, userinfo_url) = match provider {
        "google" => (
            "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            "https://oauth2.googleapis.com/token".to_string(),
            "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        ),
        "okta" => {
            let domain = env("RAGO_OKTA_DOMAIN".into())?;
            let domain = domain.trim_start_matches("https://").trim_end_matches('/').to_string();
            (
                format!("https://{domain}/oauth2/v1/authorize"),
                format!("https://{domain}/oauth2/v1/token"),
                format!("https://{domain}/oauth2/v1/userinfo"),
            )
        }
        "entra_id" => {
            let tenant = env("RAGO_ENTRA_ID_TENANT_ID".into())?;
            (
                format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
                format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                "https://graph.microsoft.com/oidc/userinfo".to_string(),
            )
        }
        other => return Err(JobError::Integration(format!("unsupported_idp: {other}"))),
    };
    Ok(Idp {
        authorize_url,
        userinfo_url,
        oauth: OAuthConfig {
            token_url,
            client_id: env(format!("RAGO_{p}_CLIENT_ID"))?,
            client_secret: env(format!("RAGO_{p}_CLIENT_SECRET"))?,
            redirect_uri: env(format!("RAGO_{p}_REDIRECT_URI"))?,
        },
    })
}

/// A started SSO login: redirect the browser to `authorize_url`.
#[derive(Debug, Clone)]
pub struct SsoStart {
    pub authorize_url: String,
    pub state: String,
}

/// Step 1 — mint and store a single-use `state` + PKCE verifier and build the
/// IdP authorize URL.
pub async fn start(db: &Db, provider: &str) -> Result<SsoStart, JobError> {
    let idp = idp(provider)?;
    let state = random_token();
    let verifier = random_token();
    let expires_at = (Utc::now() + Duration::minutes(STATE_TTL_MINUTES)).naive_utc();
    db.insert_sso_state(&state, provider, &verifier, expires_at).await?;
    let challenge = pkce_challenge(&verifier);
    let query = query_string(&[
        ("response_type", "code"),
        ("client_id", &idp.oauth.client_id),
        ("redirect_uri", &idp.oauth.redirect_uri),
        ("scope", "openid email profile"),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    Ok(SsoStart { authorize_url: format!("{}?{query}", idp.authorize_url), state })
}

/// Step 2 — verify and consume `state`, then exchange `code` with the bound
/// PKCE verifier and return the signed-in user's verified email.
pub async fn complete<T: HttpTransport>(
    db: &Db,
    transport: &T,
    provider: &str,
    code: &str,
    state: &str,
) -> Result<String, JobError> {
    if state.trim().is_empty() {
        return Err(JobError::Integration("sso_state_missing".into()));
    }
    let idp = idp(provider)?;
    let verifier = db
        .consume_sso_state(state, provider)
        .await?
        .ok_or_else(|| JobError::Integration("sso_state_invalid".into()))?;
    let tokens = OAuthClient::new(idp.oauth, transport)
        .exchange_code_with_verifier(code, &verifier)
        .await
        .map_err(|e| JobError::Integration(format!("sso_code_exchange_failed: {e}")))?;
    let headers = vec![("Authorization".to_string(), format!("Bearer {}", tokens.access_token))];
    let resp = transport
        .get(&idp.userinfo_url, &headers)
        .await
        .map_err(|e| JobError::Integration(e.to_string()))?;
    if !resp.is_success() {
        return Err(JobError::Integration(format!("sso_userinfo_failed: HTTP {}", resp.status)));
    }
    let info: Value = serde_json::from_str(&resp.body)
        .map_err(|e| JobError::Integration(format!("sso_userinfo_invalid: {e}")))?;
    if info.get("email_verified").and_then(Value::as_bool) == Some(false) {
        return Err(JobError::Integration("sso_email_not_verified".into()));
    }
    info.get("email")
        .or_else(|| info.get("preferred_username"))
        .and_then(Value::as_str)
        .filter(|e| e.contains('@'))
        .map(str::to_string)
        .ok_or_else(|| JobError::Integration("sso_email_missing".into()))
}

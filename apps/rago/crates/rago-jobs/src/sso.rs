//! SSO login — OAuth2 / OIDC authorization-code flow with Google, Okta or
//! Microsoft Entra ID, then the IdP's userinfo endpoint for the verified email.
//!
//! **Per organization.** An organization enables SSO by creating an `auth`
//! integration (`google`, `okta`, `entra_id`) whose `settings.domain` is the
//! email domain its people sign in with. Its IdP app credentials are
//! `settings.client_id` plus a sealed `client_secret` (Okta also needs
//! `organization_name` or `host`; Entra ID `tenant_id`). A deployment-wide IdP
//! app (`RAGO_<P>_CLIENT_ID` / `_CLIENT_SECRET`, `RAGO_OKTA_DOMAIN`,
//! `RAGO_ENTRA_ID_TENANT_ID`) is the fallback for integrations that carry no
//! credentials of their own — typical for one shared "Sign in with Google" app.
//!
//! The flow is server-driven and CSRF-safe:
//!
//! 1. [`start`] resolves the organization from the sign-in email's domain,
//!    mints a single-use random `state` plus a PKCE verifier bound to that
//!    organization + integration (10-minute TTL), and returns the IdP
//!    authorize URL.
//! 2. The IdP redirects back with `code` + `state`; [`complete`] atomically
//!    consumes the stored `state` (unknown, expired, reused → rejected),
//!    exchanges the code with the bound verifier and reads the verified email.
//! 3. [`resolve_login`] enforces the org policy: the user must exist, be an
//!    active member of that organization, and sign in with an email on the
//!    integration's domain. SSO never creates users.

use chrono::{Duration, Utc};
use rago_db::integration_hub::IntegrationConnection;
use rago_db::sso_state::{NewOAuthState, PURPOSE_SSO_LOGIN};
use rago_db::Db;
use rago_integrations::oauth::{pkce_challenge, query_string, random_token};
use rago_integrations::{HttpTransport, OAuthClient, OAuthConfig};
use serde_json::Value;
use uuid::Uuid;

use crate::JobError;

/// How long an issued SSO `state` stays valid.
pub const STATE_TTL_MINUTES: i64 = 10;

/// Supported IdPs.
pub const IDPS: &[&str] = &["google", "okta", "entra_id"];

struct Idp {
    authorize_url: String,
    userinfo_url: String,
    oauth: OAuthConfig,
    /// Extra authorize parameters (Google `hd` domain hint).
    hint: Option<(&'static str, String)>,
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn missing(what: String) -> JobError {
    JobError::Integration(format!("idp_not_configured: {what}"))
}

/// The redirect URI registered with the IdP: the integration's
/// `redirect_uri`, else `RAGO_<P>_REDIRECT_URI`, else `RAGO_SSO_REDIRECT_URI`,
/// else `RAGO_PUBLIC_URL` + `/admin/sso/callback` (the console's callback page).
pub fn redirect_uri(provider: &str, conn: Option<&IntegrationConnection>) -> Option<String> {
    conn.and_then(|c| c.setting("redirect_uri"))
        .or_else(|| env_opt(&format!("RAGO_{}_REDIRECT_URI", provider.to_uppercase())))
        .or_else(|| env_opt("RAGO_SSO_REDIRECT_URI"))
        .or_else(|| env_opt("RAGO_PUBLIC_URL").map(|u| format!("{}/admin/sso/callback", u.trim_end_matches('/'))))
}

fn idp(provider: &str, conn: Option<&IntegrationConnection>) -> Result<Idp, JobError> {
    let p = provider.to_uppercase();
    let setting = |key: &str| conn.and_then(|c| c.setting(key));
    let (authorize_url, token_url, userinfo_url) = match provider {
        "google" => (
            "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            "https://oauth2.googleapis.com/token".to_string(),
            "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        ),
        "okta" => {
            let host = setting("host")
                .or_else(|| setting("organization_name").map(|o| format!("{o}.okta.com")))
                .or_else(|| env_opt("RAGO_OKTA_DOMAIN"))
                .ok_or_else(|| missing("Okta organization name (or RAGO_OKTA_DOMAIN)".into()))?;
            let host = host.trim_start_matches("https://").trim_end_matches('/').to_string();
            (
                format!("https://{host}/oauth2/v1/authorize"),
                format!("https://{host}/oauth2/v1/token"),
                format!("https://{host}/oauth2/v1/userinfo"),
            )
        }
        "entra_id" => {
            let tenant = setting("tenant_id")
                .or_else(|| env_opt("RAGO_ENTRA_ID_TENANT_ID"))
                .ok_or_else(|| missing("Entra ID tenant ID (or RAGO_ENTRA_ID_TENANT_ID)".into()))?;
            (
                format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
                format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                "https://graph.microsoft.com/oidc/userinfo".to_string(),
            )
        }
        other => return Err(JobError::Integration(format!("unsupported_idp: {other}"))),
    };
    // Credentials: the integration's own IdP app, else the deployment's.
    let own_id = setting("client_id");
    let own_secret = match conn.and_then(|c| c.sealed_setting("client_secret")) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => return Err(JobError::Integration(format!("idp_secret_unavailable: {e}"))),
        None => None,
    };
    let (client_id, client_secret) = match (own_id, own_secret) {
        (Some(id), Some(secret)) => (id, secret),
        _ => (
            env_opt(&format!("RAGO_{p}_CLIENT_ID")).ok_or_else(|| missing(format!("client ID (integration or RAGO_{p}_CLIENT_ID)")))?,
            env_opt(&format!("RAGO_{p}_CLIENT_SECRET"))
                .ok_or_else(|| missing(format!("client secret (integration or RAGO_{p}_CLIENT_SECRET)")))?,
        ),
    };
    let redirect_uri = redirect_uri(provider, conn)
        .ok_or_else(|| missing(format!("redirect URI (RAGO_PUBLIC_URL or RAGO_{p}_REDIRECT_URI)")))?;
    let hint = match (provider, setting("domain")) {
        ("google", Some(domain)) => Some(("hd", domain)),
        (_, Some(domain)) => Some(("domain_hint", domain)),
        _ => None,
    };
    Ok(Idp {
        authorize_url,
        userinfo_url,
        oauth: OAuthConfig { token_url, client_id, client_secret, redirect_uri },
        hint,
    })
}

/// Whether a deployment-wide IdP app exists for `provider`.
pub fn env_idp_configured(provider: &str) -> bool {
    env_opt(&format!("RAGO_{}_CLIENT_ID", provider.to_uppercase())).is_some()
}

/// The domain part of an email (lowercased), or the input itself when it is
/// already a bare domain.
pub fn email_domain(email_or_domain: &str) -> String {
    let s = email_or_domain.trim().to_ascii_lowercase();
    match s.rsplit_once('@') {
        Some((_, d)) => d.to_string(),
        None => s,
    }
}

/// A started SSO login: redirect the browser to `authorize_url`.
#[derive(Debug, Clone)]
pub struct SsoStart {
    pub authorize_url: String,
    pub state: String,
    pub provider: String,
    pub organization_id: Option<Uuid>,
}

/// Step 1 — resolve the organization's SSO integration from the sign-in
/// email (or domain), mint and store a single-use `state` + PKCE verifier bound
/// to it, and build the IdP authorize URL.
///
/// `provider` narrows the IdP when the domain has several. Without an email
/// the deployment-wide IdP is used and the organization is resolved at
/// [`resolve_login`] (the user's org must still have enabled that IdP).
pub async fn start(db: &Db, provider: Option<&str>, email_or_domain: Option<&str>) -> Result<SsoStart, JobError> {
    let conn = match email_or_domain.map(email_domain).filter(|d| !d.is_empty()) {
        Some(domain) => {
            let found = db.auth_integrations_for_domain(&domain, provider).await?;
            Some(found.into_iter().next().ok_or_else(|| JobError::Integration("sso_not_enabled_for_domain".into()))?)
        }
        None => None,
    };
    let provider = match (&conn, provider) {
        (Some(c), _) => c.provider_code.clone(),
        (None, Some(p)) => p.to_string(),
        (None, None) => return Err(JobError::Integration("sso_email_required".into())),
    };
    if !IDPS.contains(&provider.as_str()) {
        return Err(JobError::Integration(format!("unsupported_idp: {provider}")));
    }
    let idp = idp(&provider, conn.as_ref())?;
    let state = random_token();
    let verifier = random_token();
    db.insert_oauth_state(&NewOAuthState {
        state: &state,
        purpose: PURPOSE_SSO_LOGIN,
        provider: &provider,
        code_verifier: &verifier,
        organization_id: conn.as_ref().map(|c| c.organization_id),
        integration_id: conn.as_ref().map(|c| c.id),
        expires_at: (Utc::now() + Duration::minutes(STATE_TTL_MINUTES)).naive_utc(),
    })
    .await?;
    let challenge = pkce_challenge(&verifier);
    let mut pairs: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", &idp.oauth.client_id),
        ("redirect_uri", &idp.oauth.redirect_uri),
        ("scope", "openid email profile"),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ];
    if let Some((k, v)) = &idp.hint {
        pairs.push((*k, v.as_str()));
    }
    let query = query_string(&pairs);
    Ok(SsoStart {
        authorize_url: format!("{}?{query}", idp.authorize_url),
        state,
        provider,
        organization_id: conn.map(|c| c.organization_id),
    })
}

/// The verified identity returned by the IdP, with the organization and
/// integration the login was started for.
#[derive(Debug, Clone)]
pub struct SsoIdentity {
    pub email: String,
    pub provider: String,
    pub organization_id: Option<Uuid>,
    pub integration_id: Option<Uuid>,
}

/// Step 2 — verify and consume `state`, then exchange `code` with the bound
/// PKCE verifier and return the verified identity. `provider`, when given,
/// must match the provider the state was minted for.
pub async fn complete<T: HttpTransport>(
    db: &Db,
    transport: &T,
    provider: Option<&str>,
    code: &str,
    state: &str,
) -> Result<SsoIdentity, JobError> {
    if state.trim().is_empty() {
        return Err(JobError::Integration("sso_state_missing".into()));
    }
    let row = db
        .consume_oauth_state(state, PURPOSE_SSO_LOGIN)
        .await?
        .ok_or_else(|| JobError::Integration("sso_state_invalid".into()))?;
    if provider.is_some_and(|p| p != row.provider) {
        return Err(JobError::Integration("sso_state_invalid".into()));
    }
    let conn = match (row.organization_id, row.integration_id) {
        (Some(org), Some(id)) => Some(
            db.find_org_integration(org, id)
                .await?
                .ok_or_else(|| JobError::Integration("sso_not_enabled_for_domain".into()))?,
        ),
        _ => None,
    };
    let idp = idp(&row.provider, conn.as_ref())?;
    let tokens = OAuthClient::new(idp.oauth, transport)
        .exchange_code_with_verifier(code, &row.code_verifier)
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
    let email = info
        .get("email")
        .or_else(|| info.get("preferred_username"))
        .and_then(Value::as_str)
        .filter(|e| e.contains('@'))
        .map(str::to_string)
        .ok_or_else(|| JobError::Integration("sso_email_missing".into()))?;
    Ok(SsoIdentity { email, provider: row.provider, organization_id: row.organization_id, integration_id: row.integration_id })
}

/// A signed-in user resolved from an SSO identity.
#[derive(Debug, Clone)]
pub struct SsoLogin {
    pub user_id: Uuid,
    pub email: String,
    pub organization_id: Uuid,
}

/// Step 3 — the org policy. The user must exist, be an active member of the
/// organization whose SSO integration the login went through, and sign in
/// with an email on that integration's domain. Logins started without an org
/// (deployment-wide IdP) must match one of the user's organizations that has
/// enabled this IdP for the email's domain.
pub async fn resolve_login(db: &Db, identity: &SsoIdentity) -> Result<SsoLogin, JobError> {
    let (user_id, user_email, _) = db
        .find_user_by_email(&identity.email)
        .await?
        .ok_or_else(|| JobError::Integration("user_not_found".into()))?;
    let domain = email_domain(&identity.email);

    let candidates: Vec<IntegrationConnection> = match (identity.organization_id, identity.integration_id) {
        (Some(org), Some(id)) => db.find_org_integration(org, id).await?.into_iter().collect(),
        _ => db.auth_integrations_for_domain(&domain, Some(&identity.provider)).await?,
    };
    for conn in candidates {
        if conn.provider_code != identity.provider {
            continue;
        }
        let domain_ok = conn.setting("domain").map(|d| email_domain(&d) == domain).unwrap_or(false);
        if !domain_ok {
            continue;
        }
        if db.is_active_member(conn.organization_id, user_id).await? {
            return Ok(SsoLogin { user_id, email: user_email, organization_id: conn.organization_id });
        }
    }
    Err(JobError::Integration("sso_not_enabled_for_user".into()))
}

/// Whether `user_id` may sign in with a password: `false` when any of the
/// user's organizations enforces SSO (`settings.enforce_sso = true` on one of
/// its auth integrations).
pub async fn password_login_allowed(db: &Db, user_id: Uuid) -> Result<bool, JobError> {
    for org in db.organizations_of_user(user_id).await? {
        for conn in db.list_org_integrations(org).await? {
            if conn.integration_type == "auth" && conn.flag("enforce_sso") {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

//! Per-organization OAuth for accounting / CRM integrations (Xero,
//! Salesforce, HubSpot): the authorize → callback → token lifecycle and
//! automatic token refresh before provider calls.
//!
//! Each provider has a fixed endpoint catalog ([`provider_oauth`]). The OAuth
//! *app* credentials resolve per integration first — `client_id` plus a sealed
//! `client_secret` saved on the integration's settings — and fall back to the
//! deployment-wide `RAGO_<PROVIDER>_OAUTH_CLIENT_ID` / `_SECRET` (and the
//! legacy `RAGO_OAUTH_CLIENT_ID` / `_SECRET`). Nothing is fabricated: a missing
//! credential is a structured `oauth_not_configured` error naming the setting.
//!
//! The flow is CSRF-safe: [`authorize`] mints a single-use `state` bound to
//! the organization and integration (plus a PKCE verifier where the provider
//! supports it); [`complete`] atomically consumes it, exchanges the code, and
//! stores the tokens and the provider addressing data (Xero tenant id,
//! Salesforce `instance_url`, HubSpot portal id) on that same integration row.

use chrono::{Duration, Utc};
use rago_db::integration_hub::IntegrationConnection;
use rago_db::sso_state::{NewOAuthState, PURPOSE_INTEGRATION_OAUTH};
use rago_db::Db;
use rago_integrations::oauth::{pkce_challenge, query_string, random_token};
use rago_integrations::{HttpTransport, OAuthClient, OAuthConfig, OAuthTokens, XeroProvider};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::JobError;

/// How long an integration OAuth `state` stays valid.
pub const STATE_TTL_MINUTES: i64 = 15;
/// Refresh an access token this long before it expires.
pub const REFRESH_MARGIN_SECONDS: i64 = 120;

/// Providers connected through an OAuth authorization-code grant.
pub const OAUTH_PROVIDERS: &[&str] = &["xero", "salesforce", "hubspot"];

/// Whether `provider_code` connects through OAuth.
pub fn is_oauth_provider(provider_code: &str) -> bool {
    OAUTH_PROVIDERS.contains(&provider_code)
}

/// One provider's OAuth endpoints and requested scopes.
#[derive(Debug, Clone)]
pub struct ProviderOAuth {
    pub authorize_url: String,
    pub token_url: String,
    pub scopes: &'static str,
    /// Whether the provider accepts PKCE (`S256`).
    pub pkce: bool,
}

/// The endpoint catalog for `conn`'s provider.
pub fn provider_oauth(conn: &IntegrationConnection) -> Result<ProviderOAuth, JobError> {
    match conn.provider_code.as_str() {
        "xero" => Ok(ProviderOAuth {
            authorize_url: "https://login.xero.com/identity/connect/authorize".into(),
            token_url: "https://identity.xero.com/connect/token".into(),
            scopes: "openid profile email offline_access accounting.transactions accounting.contacts accounting.settings",
            pkce: true,
        }),
        "salesforce" => {
            // Sandboxes authorize at test.salesforce.com; My Domain orgs may
            // pin their own login host.
            let host = conn
                .setting("login_url")
                .unwrap_or_else(|| {
                    if conn.flag("sandbox") { "https://test.salesforce.com".into() } else { "https://login.salesforce.com".into() }
                });
            let host = host.trim_end_matches('/').to_string();
            Ok(ProviderOAuth {
                authorize_url: format!("{host}/services/oauth2/authorize"),
                token_url: format!("{host}/services/oauth2/token"),
                scopes: "api refresh_token offline_access",
                pkce: true,
            })
        }
        "hubspot" => Ok(ProviderOAuth {
            authorize_url: "https://app.hubspot.com/oauth/authorize".into(),
            token_url: "https://api.hubapi.com/oauth/v1/token".into(),
            scopes: "oauth crm.objects.contacts.read crm.objects.contacts.write crm.objects.companies.read crm.objects.companies.write",
            pkce: false,
        }),
        other => Err(JobError::Integration(format!("oauth_unsupported_provider: {other}"))),
    }
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The OAuth app credentials `(client_id, client_secret)` for `conn`.
pub fn client_credentials(conn: &IntegrationConnection) -> Result<(String, String), JobError> {
    let p = conn.provider_code.to_ascii_uppercase();
    let client_id = conn
        .setting("client_id")
        .or_else(|| env_opt(&format!("RAGO_{p}_OAUTH_CLIENT_ID")))
        .or_else(|| env_opt("RAGO_OAUTH_CLIENT_ID"));
    let client_secret = match conn.sealed_setting("client_secret") {
        Some(Ok(secret)) => Some(secret),
        Some(Err(e)) => {
            return Err(JobError::Integration(format!("{}_client_secret_unavailable: {e}", conn.provider_code)))
        }
        None => env_opt(&format!("RAGO_{p}_OAUTH_CLIENT_SECRET")).or_else(|| env_opt("RAGO_OAUTH_CLIENT_SECRET")),
    };
    match (client_id, client_secret) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        _ => Err(JobError::Integration(format!(
            "oauth_not_configured: save the {} app's client ID and secret on the integration, or set RAGO_{p}_OAUTH_CLIENT_ID and RAGO_{p}_OAUTH_CLIENT_SECRET",
            conn.provider_code
        ))),
    }
}

/// The callback URL registered with every provider app:
/// `RAGO_INTEGRATION_OAUTH_REDIRECT_URI`, else `RAGO_OAUTH_REDIRECT_URI`, else
/// `RAGO_PUBLIC_URL` + `/integrations/oauth/callback`.
pub fn redirect_uri() -> Result<String, JobError> {
    env_opt("RAGO_INTEGRATION_OAUTH_REDIRECT_URI")
        .or_else(|| env_opt("RAGO_OAUTH_REDIRECT_URI"))
        .or_else(|| env_opt("RAGO_PUBLIC_URL").map(|u| format!("{}/integrations/oauth/callback", u.trim_end_matches('/'))))
        .ok_or_else(|| {
            JobError::Integration(
                "oauth_not_configured: set RAGO_PUBLIC_URL (or RAGO_INTEGRATION_OAUTH_REDIRECT_URI) so providers can redirect back".into(),
            )
        })
}

/// The OAuth client config for `conn`.
pub fn oauth_config(conn: &IntegrationConnection, with_redirect: bool) -> Result<OAuthConfig, JobError> {
    let endpoints = provider_oauth(conn)?;
    let (client_id, client_secret) = client_credentials(conn)?;
    Ok(OAuthConfig {
        token_url: endpoints.token_url,
        client_id,
        client_secret,
        redirect_uri: if with_redirect { redirect_uri()? } else { String::new() },
    })
}

/// Step 1 — mint a single-use `state` bound to the org + integration and
/// return the provider authorize URL.
pub async fn authorize(db: &Db, conn: &IntegrationConnection) -> Result<String, JobError> {
    let endpoints = provider_oauth(conn)?;
    let config = oauth_config(conn, true)?;
    let state = random_token();
    let verifier = random_token();
    db.insert_oauth_state(&NewOAuthState {
        state: &state,
        purpose: PURPOSE_INTEGRATION_OAUTH,
        provider: &conn.provider_code,
        code_verifier: &verifier,
        organization_id: Some(conn.organization_id),
        integration_id: Some(conn.id),
        expires_at: (Utc::now() + Duration::minutes(STATE_TTL_MINUTES)).naive_utc(),
    })
    .await?;
    let challenge = pkce_challenge(&verifier);
    let mut pairs: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", &config.client_id),
        ("redirect_uri", &config.redirect_uri),
        ("scope", endpoints.scopes),
        ("state", &state),
    ];
    if endpoints.pkce {
        pairs.push(("code_challenge", &challenge));
        pairs.push(("code_challenge_method", "S256"));
    }
    Ok(format!("{}?{}", endpoints.authorize_url, query_string(&pairs)))
}

/// The outcome of a completed integration authorization.
#[derive(Debug, Clone)]
pub struct Connected {
    pub organization_id: Uuid,
    pub integration_id: Uuid,
    pub provider_code: String,
}

/// Step 2 — consume `state`, exchange `code`, capture provider addressing
/// data, and store the tokens on the integration the state was minted for.
pub async fn complete<T: HttpTransport>(
    db: &Db,
    transport: &T,
    code: &str,
    state: &str,
) -> Result<Connected, JobError> {
    if state.trim().is_empty() || code.trim().is_empty() {
        return Err(JobError::Integration("oauth_state_or_code_missing".into()));
    }
    let row = db
        .consume_oauth_state(state, PURPOSE_INTEGRATION_OAUTH)
        .await?
        .ok_or_else(|| JobError::Integration("oauth_state_invalid".into()))?;
    let (org, integration_id) = match (row.organization_id, row.integration_id) {
        (Some(o), Some(i)) => (o, i),
        _ => return Err(JobError::Integration("oauth_state_invalid".into())),
    };
    let conn = db
        .find_org_integration(org, integration_id)
        .await?
        .ok_or_else(|| JobError::Integration("integration_not_found".into()))?;
    if conn.provider_code != row.provider {
        return Err(JobError::Integration("oauth_state_invalid".into()));
    }
    let endpoints = provider_oauth(&conn)?;
    let client = OAuthClient::new(oauth_config(&conn, true)?, transport);
    let tokens = if endpoints.pkce {
        client.exchange_code_with_verifier(code, &row.code_verifier).await
    } else {
        client.exchange_code(code).await
    }
    .map_err(|e| JobError::Integration(format!("oauth_code_exchange_failed: {e}")))?;

    let mut settings = conn.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    capture_addressing(transport, &conn, &tokens, &mut settings).await?;
    settings["connected_at"] = json!(Utc::now().to_rfc3339());
    if let Some(scope) = &tokens.scope {
        settings["granted_scope"] = json!(scope);
    }
    let expires_at = tokens.expires_in.map(|s| (Utc::now() + Duration::seconds(s)).naive_utc());
    db.update_integration_tokens(conn.id, &tokens.access_token, tokens.refresh_token.as_deref(), expires_at)
        .await?;
    db.replace_integration_settings(conn.id, &settings).await?;
    Ok(Connected { organization_id: org, integration_id: conn.id, provider_code: conn.provider_code })
}

/// Record what later API calls need to address the connected account.
async fn capture_addressing<T: HttpTransport>(
    transport: &T,
    conn: &IntegrationConnection,
    tokens: &OAuthTokens,
    settings: &mut Value,
) -> Result<(), JobError> {
    match conn.provider_code.as_str() {
        "xero" => {
            let tenants = XeroProvider::direct(tokens.access_token.clone(), "", transport)
                .connections()
                .await
                .map_err(|e| JobError::Integration(format!("xero_connections_failed: {e}")))?;
            // Keep a previously chosen tenant when it is still connected.
            let wanted = conn.setting("tenant_id");
            let chosen = tenants
                .iter()
                .find(|(id, _)| Some(id) == wanted.as_ref())
                .or_else(|| tenants.first())
                .cloned()
                .ok_or_else(|| JobError::Integration("xero_no_tenant_authorized".into()))?;
            settings["tenant_id"] = json!(chosen.0);
            settings["tenant_name"] = json!(chosen.1);
            settings["available_tenants"] =
                json!(tenants.iter().map(|(id, name)| json!({ "tenant_id": id, "tenant_name": name })).collect::<Vec<_>>());
        }
        "salesforce" => {
            let instance = tokens
                .instance_url
                .clone()
                .ok_or_else(|| JobError::Integration("salesforce_instance_url_missing".into()))?;
            settings["instance_url"] = json!(instance);
        }
        "hubspot" => {
            let url = format!("https://api.hubapi.com/oauth/v1/access-tokens/{}", tokens.access_token);
            match transport.get(&url, &[]).await {
                Ok(resp) if resp.is_success() => {
                    let info: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
                    settings["portal_id"] = info.get("hub_id").cloned().unwrap_or(Value::Null);
                    settings["hub_domain"] = info.get("hub_domain").cloned().unwrap_or(Value::Null);
                }
                Ok(resp) => tracing::warn!(status = resp.status, "hubspot token info lookup failed"),
                Err(e) => tracing::warn!(error = %e, "hubspot token info lookup failed"),
            }
        }
        _ => {}
    }
    Ok(())
}

/// Refresh `conn`'s access token with its stored refresh token and persist
/// the new tokens.
pub async fn refresh<T: HttpTransport>(
    db: &Db,
    transport: &T,
    conn: &IntegrationConnection,
) -> Result<OAuthTokens, JobError> {
    let refresh_token = conn
        .refresh_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| JobError::Integration(format!("{}_refresh_token_missing: reconnect the integration", conn.provider_code)))?;
    let tokens = OAuthClient::new(oauth_config(conn, false)?, transport)
        .refresh(&refresh_token)
        .await
        .map_err(|e| JobError::Integration(format!("oauth_refresh_failed: {e}")))?;
    let expires_at = tokens.expires_in.map(|s| (Utc::now() + Duration::seconds(s)).naive_utc());
    db.update_integration_tokens(conn.id, &tokens.access_token, tokens.refresh_token.as_deref(), expires_at)
        .await?;
    if let Some(instance) = &tokens.instance_url {
        let mut settings = conn.settings.clone();
        if settings.get("instance_url").and_then(Value::as_str) != Some(instance.as_str()) {
            settings["instance_url"] = json!(instance);
            db.replace_integration_settings(conn.id, &settings).await?;
        }
    }
    Ok(tokens)
}

/// A usable access token for `conn`, refreshed first when it expires within
/// [`REFRESH_MARGIN_SECONDS`].
pub async fn fresh_access_token<T: HttpTransport>(
    db: &Db,
    transport: &T,
    conn: &IntegrationConnection,
) -> Result<String, JobError> {
    let token = conn
        .access_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| JobError::Integration(format!("{}_not_authorized: connect the integration from the console", conn.provider_code)))?;
    let expiring = conn
        .token_expires_at
        .is_some_and(|exp| exp <= (Utc::now() + Duration::seconds(REFRESH_MARGIN_SECONDS)).naive_utc());
    if expiring && is_oauth_provider(&conn.provider_code) && conn.refresh_token.is_some() {
        return Ok(refresh(db, transport, conn).await?.access_token);
    }
    Ok(token)
}

/// Periodic maintenance: refresh every OAuth token expiring within ten
/// minutes. Failures are logged per connection (the next tick retries).
/// Returns the number refreshed.
pub async fn refresh_expiring_tokens<T: HttpTransport>(db: &Db, transport: &T) -> Result<u32, JobError> {
    let horizon = (Utc::now() + Duration::minutes(10)).naive_utc();
    let mut refreshed = 0u32;
    for conn in db.integrations_expiring_before(horizon).await? {
        if !is_oauth_provider(&conn.provider_code) {
            continue;
        }
        match refresh(db, transport, &conn).await {
            Ok(_) => refreshed += 1,
            Err(e) => tracing::warn!(integration_id = %conn.id, provider = %conn.provider_code, error = %e, "integration token refresh failed"),
        }
    }
    Ok(refreshed)
}

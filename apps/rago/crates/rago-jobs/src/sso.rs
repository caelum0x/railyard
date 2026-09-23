//! SSO login — OAuth2 / OIDC authorization-code exchange with Google, Okta or
//! Microsoft Entra ID, then the IdP's userinfo endpoint for the verified email.
//!
//! Env per provider (`GOOGLE`, `OKTA`, `ENTRA_ID`):
//! `RAGO_<P>_CLIENT_ID`, `RAGO_<P>_CLIENT_SECRET`, `RAGO_<P>_REDIRECT_URI`, plus
//! `RAGO_OKTA_DOMAIN` (e.g. `acme.okta.com`) or `RAGO_ENTRA_ID_TENANT_ID`.

use rago_integrations::{HttpTransport, OAuthClient, OAuthConfig};
use serde_json::Value;

use crate::JobError;

struct Idp {
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
    let (token_url, userinfo_url) = match provider {
        "google" => (
            "https://oauth2.googleapis.com/token".to_string(),
            "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        ),
        "okta" => {
            let domain = env("RAGO_OKTA_DOMAIN".into())?;
            let domain = domain.trim_start_matches("https://").trim_end_matches('/').to_string();
            (
                format!("https://{domain}/oauth2/v1/token"),
                format!("https://{domain}/oauth2/v1/userinfo"),
            )
        }
        "entra_id" => {
            let tenant = env("RAGO_ENTRA_ID_TENANT_ID".into())?;
            (
                format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                "https://graph.microsoft.com/oidc/userinfo".to_string(),
            )
        }
        other => return Err(JobError::Integration(format!("unsupported_idp: {other}"))),
    };
    Ok(Idp {
        userinfo_url,
        oauth: OAuthConfig {
            token_url,
            client_id: env(format!("RAGO_{p}_CLIENT_ID"))?,
            client_secret: env(format!("RAGO_{p}_CLIENT_SECRET"))?,
            redirect_uri: env(format!("RAGO_{p}_REDIRECT_URI"))?,
        },
    })
}

/// Exchange `code` with the IdP and return the signed-in user's verified email.
pub async fn verified_email<T: HttpTransport>(transport: &T, provider: &str, code: &str) -> Result<String, JobError> {
    let idp = idp(provider)?;
    let tokens = OAuthClient::new(idp.oauth, transport)
        .exchange_code(code)
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

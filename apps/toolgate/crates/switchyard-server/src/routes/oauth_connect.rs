//! Connect a team's own provider account over OAuth 2 (authorization code +
//! PKCE S256), so `byo` tools call the provider as that team.
//!
//! * `POST /v1/credentials/oauth/{provider}/connect` (admin): returns the
//!   provider's authorize URL. The `state` is a signed, expiring token that
//!   carries the org, provider and credential name; the PKCE verifier is
//!   derived from it, so no server-side session is needed.
//! * `GET /v1/credentials/oauth/{provider}/callback?code&state`: verifies the
//!   state, exchanges the code, and seals the access token as the team's
//!   `oauth` credential (expiry in `meta`) and the refresh token as a separate
//!   sealed credential. [`refresh_if_due`] renews the access token before a
//!   call when it is about to expire.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use base64::Engine;
use chrono::Utc;
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use switchyard_auth::scopes;
use switchyard_core::{ApiError, Credential, CredentialKind, HttpRequest, OAuthSpec};
use switchyard_store::CredentialRow;

use super::tools::principal;
use crate::http_util::{ok, ApiResult};
use crate::state::AppState;

/// How long a connect link stays valid.
const STATE_TTL_SECS: i64 = 600;
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 60;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

/// The refresh token's credential provider id.
pub fn refresh_provider(provider: &str) -> String {
    format!("{provider}#refresh")
}

fn sign_state(secret: &str, payload: &Value) -> String {
    let body = B64.encode(payload.to_string());
    let mac = B64.encode(hmac_sha256(secret.as_bytes(), body.as_bytes()));
    format!("{body}.{mac}")
}

fn verify_state(secret: &str, state: &str, now: i64) -> Option<Value> {
    let (body, mac) = state.split_once('.')?;
    let expected = B64.encode(hmac_sha256(secret.as_bytes(), body.as_bytes()));
    // Constant-time comparison.
    if expected.len() != mac.len() || expected.bytes().zip(mac.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        return None;
    }
    let payload: Value = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
    (payload.get("exp")?.as_i64()? > now).then_some(payload)
}

fn pkce_verifier(secret: &str, nonce: &str) -> String {
    B64.encode(hmac_sha256(secret.as_bytes(), format!("pkce|{nonce}").as_bytes()))
}

fn redirect_uri(st: &AppState, provider: &str) -> String {
    format!("{}/v1/credentials/oauth/{provider}/callback", st.settings.public_url)
}

fn spec(st: &AppState, provider: &str) -> Result<(OAuthSpec, crate::config::OAuthClient), ApiError> {
    let spec = st
        .catalog
        .provider(provider)
        .and_then(|p| p.oauth.clone())
        .ok_or_else(|| ApiError::not_found(format!("{provider} has no OAuth connect")))?;
    let client = st
        .settings
        .oauth_clients
        .get(provider)
        .cloned()
        .ok_or_else(|| ApiError::new(503, "oauth_not_configured", format!("OAuth for {provider} is not configured on this Switchyard")))?;
    Ok((spec, client))
}

#[derive(Deserialize, Default)]
pub struct ConnectBody {
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /v1/credentials/oauth/{provider}/connect`
pub async fn connect(State(st): State<AppState>, headers: HeaderMap, Path(provider): Path<String>, body: Bytes) -> ApiResult {
    let p = principal(&st, &headers).await?;
    p.require(scopes::ADMIN)?;
    let (spec, client) = spec(&st, &provider)?;
    let b: ConnectBody = if body.is_empty() { ConnectBody::default() } else { serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))? };
    let name = b.name.filter(|n| !n.trim().is_empty() && n.len() <= 100).unwrap_or_else(|| provider.clone());
    let nonce = switchyard_auth::random_token();
    let exp = Utc::now().timestamp() + STATE_TTL_SECS;
    let state = sign_state(&st.settings.form_secret, &json!({ "org": p.org.id, "provider": provider, "name": name, "nonce": nonce, "exp": exp }));
    let challenge = B64.encode(Sha256::digest(pkce_verifier(&st.settings.form_secret, &nonce)));
    let mut url = url::Url::parse(&spec.authorize_url).map_err(|_| ApiError::internal("provider authorize_url is invalid"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", &redirect_uri(&st, &provider))
        .append_pair("scope", &spec.scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(ok(json!({ "authorize_url": url.to_string(), "expires_in": STATE_TTL_SECS })))
}

/// POST a form to the token endpoint; returns the token response JSON.
async fn token_request(st: &AppState, spec: &OAuthSpec, form: &[(&str, &str)]) -> Result<Value, ApiError> {
    let url = url::Url::parse(&spec.token_url).map_err(|_| ApiError::internal("provider token_url is invalid"))?;
    let body: String = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
    let mut req = HttpRequest::get(spec.token_url.clone()).header("content-type", "application/x-www-form-urlencoded").header("accept", "application/json");
    req.method = "POST".into();
    req.body = Some(body.into_bytes());
    req.extra_hosts = url.host_str().map(|h| vec![h.to_string()]).unwrap_or_default();
    let resp = st.http.send(req).await.map_err(|e| ApiError::new(502, "oauth_token_failed", format!("the token endpoint could not be reached: {e}")))?;
    let v = resp.json().map_err(|_| ApiError::new(502, "oauth_token_failed", "the token endpoint did not answer JSON"))?;
    if !(200..300).contains(&resp.status) || v.get("access_token").and_then(Value::as_str).is_none() {
        let reason = v.get("error").and_then(Value::as_str).unwrap_or("no access_token");
        return Err(ApiError::new(502, "oauth_token_failed", format!("the provider refused the exchange: {reason}")));
    }
    Ok(v)
}

/// Seal a token response as the access credential (+ refresh credential).
async fn store_tokens(st: &AppState, org: &str, provider: &str, name: &str, tokens: &Value) -> Result<CredentialRow, ApiError> {
    let access = tokens.get("access_token").and_then(Value::as_str).unwrap_or_default();
    let expires_at = tokens.get("expires_in").and_then(Value::as_i64).map(|s| Utc::now().timestamp() + s);
    let meta = json!({ "oauth": true, "expires_at": expires_at, "scope": tokens.get("scope"), "token_type": tokens.get("token_type") });
    let row = crate::vaultops::store_credential(st, org, provider, name, CredentialKind::Oauth, access, meta).await?;
    if let Some(refresh) = tokens.get("refresh_token").and_then(Value::as_str) {
        crate::vaultops::store_credential(st, org, &refresh_provider(provider), name, CredentialKind::Oauth, refresh, json!({ "for": row.id })).await?;
    }
    Ok(row)
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

fn page(title: &str, text: &str) -> axum::response::Response {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    Html(format!("<!doctype html><meta charset=utf-8><title>{t}</title><h1>{t}</h1><p>{x}</p>", t = esc(title), x = esc(text))).into_response()
}

/// `GET /v1/credentials/oauth/{provider}/callback`
pub async fn callback(State(st): State<AppState>, Path(provider): Path<String>, Query(q): Query<CallbackQuery>) -> axum::response::Response {
    if let Some(e) = q.error {
        return (axum::http::StatusCode::BAD_REQUEST, page("Not connected", &format!("The provider answered: {e}"))).into_response();
    }
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return (axum::http::StatusCode::BAD_REQUEST, page("Not connected", "The link is missing its code or state.")).into_response();
    };
    let Some(payload) = verify_state(&st.settings.form_secret, &state, Utc::now().timestamp()) else {
        return (axum::http::StatusCode::BAD_REQUEST, page("Not connected", "This connect link is invalid or expired. Start again.")).into_response();
    };
    if payload["provider"].as_str() != Some(provider.as_str()) {
        return (axum::http::StatusCode::BAD_REQUEST, page("Not connected", "This link belongs to another provider.")).into_response();
    }
    let (org, name, nonce) = (payload["org"].as_str().unwrap_or_default().to_string(), payload["name"].as_str().unwrap_or_default().to_string(), payload["nonce"].as_str().unwrap_or_default().to_string());
    let result = async {
        let (spec, client) = spec(&st, &provider)?;
        let verifier = pkce_verifier(&st.settings.form_secret, &nonce);
        let redirect = redirect_uri(&st, &provider);
        let tokens = token_request(&st, &spec, &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("client_id", &client.client_id), ("client_secret", &client.client_secret), ("code_verifier", &verifier)]).await?;
        store_tokens(&st, &org, &provider, &name, &tokens).await
    }
    .await;
    match result {
        Ok(_) => page("Connected", &format!("Your {provider} account is connected. You can close this window.")),
        Err(e) => (axum::http::StatusCode::from_u16(e.status).unwrap_or(axum::http::StatusCode::BAD_GATEWAY), page("Not connected", &e.message)).into_response(),
    }
}

/// Renew an `oauth` credential that is about to expire, using its stored
/// refresh token. Returns the credential to use (renewed or as it was).
pub async fn refresh_if_due(st: &AppState, org: &str, cred: Credential) -> Result<Credential, String> {
    let due = cred.kind == CredentialKind::Oauth && cred.meta.get("expires_at").and_then(Value::as_i64).is_some_and(|exp| exp - Utc::now().timestamp() < REFRESH_MARGIN_SECS);
    if !due || cred.platform {
        return Ok(cred);
    }
    let refresh_row = match st.store.find_credential(org, &refresh_provider(&cred.provider)).await {
        Ok(Some(r)) => r,
        _ => return Ok(cred), // no refresh token: use it as is; the provider decides
    };
    let refresh = crate::vaultops::open_by_id(st, org, &refresh_row.id).await.map_err(|e| e.message)?;
    let (spec, client) = spec(st, &cred.provider).map_err(|e| e.message)?;
    let tokens = token_request(st, &spec, &[("grant_type", "refresh_token"), ("refresh_token", refresh.secret.expose_secret()), ("client_id", &client.client_id), ("client_secret", &client.client_secret)])
        .await
        .map_err(|e| e.message)?;
    // Keep the refresh token when the provider does not rotate it.
    let mut tokens = tokens;
    if tokens.get("refresh_token").is_none() {
        tokens["refresh_token"] = json!(refresh.secret.expose_secret());
    }
    let name = st.store.get_credential(org, &cred.id).await.ok().flatten().map(|r| r.name).unwrap_or_else(|| cred.provider.clone());
    let row = store_tokens(st, org, &cred.provider, &name, &tokens).await.map_err(|e| e.message)?;
    let _ = st.store.delete_credential(org, &cred.id).await;
    let _ = st.store.delete_credential(org, &refresh_row.id).await;
    crate::vaultops::open_by_id(st, org, &row.id).await.map_err(|e| e.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn states_are_signed_and_expire() {
        let s = sign_state("k", &json!({ "org": "o", "exp": 100 }));
        assert_eq!(verify_state("k", &s, 50).unwrap()["org"], "o");
        assert!(verify_state("k", &s, 100).is_none(), "expired");
        assert!(verify_state("other", &s, 50).is_none(), "wrong key");
        let (body, mac) = s.split_once('.').unwrap();
        let forged = format!("{}.{mac}", B64.encode(r#"{"org":"evil","exp":100}"#));
        assert!(verify_state("k", &forged, 50).is_none(), "tampered body {body}");
    }
}

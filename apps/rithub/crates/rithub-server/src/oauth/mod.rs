//! OAuth2 / SSO login over the standard authorization-code flow.
//!
//! Two routes drive the flow:
//!
//! - `GET /login/oauth/{provider}` — mint an anti-CSRF `state` and a PKCE
//!   verifier/challenge pair, stash them in a short-lived `HttpOnly` cookie, and
//!   redirect the browser to the provider's authorize endpoint.
//! - `GET /login/oauth/{provider}/callback` — validate `state` against the
//!   cookie, exchange the `code` for an access token, fetch userinfo, then
//!   **find-or-create** the forge user, **link** the external identity, mint a
//!   session token, and set the `rithub_session` cookie (the same format the
//!   password login uses, so the rest of the web UI is unchanged).
//!
//! ## Security posture
//! - **State**: the callback rejects any request whose `state` query does not
//!   equal the value in the `HttpOnly` cookie set at the start of the flow
//!   (double-submit CSRF defense — an attacker cannot set the victim's cookie).
//! - **PKCE**: an S256 `code_challenge` is sent on authorize and the matching
//!   verifier on exchange, binding the code to this browser.
//! - **Short-lived code exchange**: the transient cookie is `Max-Age=600`, and
//!   the authorization code is single-use and exchanged immediately server-side.
//! - **No secret logging**: client secrets and access tokens never appear in
//!   logs; only provider slugs and coarse failure reasons are recorded.

mod provider;

pub use provider::{is_enabled, OauthProvider, OauthUserInfo};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use rithub_core::{validate_name, CoreError, OauthStore, Store, User};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::state::AppState;
use crate::web;
use provider::{provider_for, OauthError};

/// Name of the transient cookie holding `provider:state:verifier`.
const OAUTH_COOKIE: &str = "rithub_oauth";

/// Register the OAuth login routes. Merged into the main router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login/oauth/{provider}", get(start))
        .route("/login/oauth/{provider}/callback", get(callback))
}

// ---------------------------------------------------------------------------
// Start: redirect to the provider
// ---------------------------------------------------------------------------

async fn start(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    let provider = provider.to_ascii_lowercase();
    let prov = match provider_for(&provider) {
        Ok(p) => p,
        Err(e) => return provider_error_page(&provider, &e),
    };

    let redirect_uri = callback_uri(&headers, &provider);
    let csrf_state = random_token();
    let (verifier, challenge) = pkce_pair();
    let authorize_url = prov.authorize_url(&redirect_uri, &csrf_state, &challenge);
    if authorize_url.is_empty() {
        tracing::error!(
            provider,
            "authorize URL could not be built (bad auth_url config)"
        );
        return web::error_page(
            &None,
            StatusCode::INTERNAL_SERVER_ERROR,
            "OAuth provider is misconfigured.",
        );
    }

    // Stash provider + state + PKCE verifier for the callback to validate.
    let stash = STANDARD.encode(format!("{provider}:{csrf_state}:{verifier}"));
    let set_cookie = format!("{OAUTH_COOKIE}={stash}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600");
    (
        AppendHeaders([(header::SET_COOKIE, set_cookie)]),
        Redirect::to(&authorize_url),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Callback: exchange code, resolve user, open a session
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    /// Set by the provider when the user denies consent or an error occurs.
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    let provider = provider.to_ascii_lowercase();

    if let Some(err) = params.error.as_deref() {
        tracing::info!(provider, error = err, "oauth provider returned an error");
        return web::error_page(
            &None,
            StatusCode::UNAUTHORIZED,
            "Sign-in was cancelled or denied by the provider.",
        );
    }

    let (Some(code), Some(returned_state)) = (params.code, params.state) else {
        return web::error_page(
            &None,
            StatusCode::BAD_REQUEST,
            "Missing authorization code or state.",
        );
    };

    // Validate state (and recover the PKCE verifier) from the transient cookie.
    let Some((cookie_provider, cookie_state, verifier)) = read_oauth_cookie(&headers) else {
        return web::error_page(
            &None,
            StatusCode::BAD_REQUEST,
            "The sign-in session expired. Please try again.",
        );
    };
    if cookie_provider != provider || cookie_state != returned_state {
        tracing::warn!(provider, "oauth state mismatch — possible CSRF");
        return clear_and_fail(
            StatusCode::BAD_REQUEST,
            "Invalid sign-in state. Please try again.",
        );
    }

    let prov = match provider_for(&provider) {
        Ok(p) => p,
        Err(e) => return provider_error_page(&provider, &e),
    };

    // The redirect_uri must match the one used on the authorize step exactly.
    let redirect_uri = callback_uri(&headers, &provider);

    let access_token = match prov.exchange_code(&redirect_uri, &code, &verifier).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(provider, error = %e, "oauth token exchange failed");
            return clear_and_fail(
                StatusCode::BAD_GATEWAY,
                "Could not complete sign-in with the provider.",
            );
        }
    };

    let info = match prov.fetch_userinfo(&access_token).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(provider, error = %e, "oauth userinfo fetch failed");
            return clear_and_fail(
                StatusCode::BAD_GATEWAY,
                "Could not read your profile from the provider.",
            );
        }
    };

    let user = match resolve_user(&state, &provider, &info).await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(provider, error = %e, "oauth user resolution failed");
            return clear_and_fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not establish your account.",
            );
        }
    };

    // Open a server-side, revocable web session (never a full-access PAT).
    let session_cookie = match web::start_session_cookie(&state, &user).await {
        Ok(cookie) => cookie,
        Err(_) => {
            tracing::error!(provider, "starting oauth web session failed");
            return clear_and_fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start your session.",
            );
        }
    };

    crate::audit::record(
        &state,
        None,
        Some(user.id),
        &user.username,
        "auth.oauth.login",
        Some(&provider),
        None,
    )
    .await;

    let clear_oauth = format!("{OAUTH_COOKIE}=; Path=/; HttpOnly; Max-Age=0");
    (
        AppendHeaders([
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear_oauth),
        ]),
        Redirect::to("/"),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Find-or-create + link
// ---------------------------------------------------------------------------

/// Resolve the forge user for a provider identity, creating and/or linking as
/// needed:
/// 1. If the `(provider, subject)` is already linked, return that user.
/// 2. Else, if a user with the same email exists, link the identity to them.
/// 3. Else, create a fresh user (unique username derived from the profile) and
///    link the identity.
async fn resolve_user(
    state: &AppState,
    provider: &str,
    info: &OauthUserInfo,
) -> Result<User, CoreError> {
    // 1. Already linked?
    if let Some(identity) = state
        .store
        .find_oauth_identity(provider, &info.provider_user_id)
        .await?
    {
        if let Some(user) = state.store.get_user_by_id(identity.user_id).await? {
            return Ok(user);
        }
        // The linked user vanished (should be prevented by ON DELETE CASCADE);
        // fall through and re-establish an account.
    }

    // 2. Link to an existing account by email, or 3. create a new one.
    let user = match nonempty(&info.email) {
        Some(email) => match state.store.find_user_by_email(email).await? {
            Some(existing) => existing,
            None => create_user_from_oauth(state, info).await?,
        },
        None => create_user_from_oauth(state, info).await?,
    };

    state
        .store
        .create_oauth_identity(
            user.id,
            provider,
            &info.provider_user_id,
            nonempty(&info.email),
        )
        .await?;
    Ok(user)
}

/// Create a brand-new forge user for an OAuth login, allocating a unique,
/// validation-passing username derived from the provider profile.
async fn create_user_from_oauth(state: &AppState, info: &OauthUserInfo) -> Result<User, CoreError> {
    let base = slugify(&info.username, &info.email);
    let has_real_email = nonempty(&info.email).is_some();

    // Try `base`, then `base-1`, `base-2`, ... resolving username/email
    // collisions. Each attempt also gets a distinct synthetic email when the
    // provider withheld one, so the users.email UNIQUE constraint can't wedge us.
    for attempt in 0..64u32 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        if validate_name("user", &candidate).is_err() {
            continue;
        }
        let email = if has_real_email {
            info.email.clone()
        } else {
            format!("{candidate}@oauth.local")
        };
        match state.store.create_user(&candidate, &email).await {
            Ok(user) => {
                crate::api::ensure_personal_org(state, &user).await;
                return Ok(user);
            }
            // Username or email already taken — try the next candidate.
            Err(CoreError::Conflict { .. }) => continue,
            Err(other) => return Err(other),
        }
    }
    Err(CoreError::Database(
        "could not allocate a unique username for oauth user".to_string(),
    ))
}

/// Derive a forge-legal username seed from a profile handle or email local part.
/// The result still passes through [`validate_name`] at creation time.
fn slugify(username: &str, email: &str) -> String {
    let seed = if !username.trim().is_empty() {
        username
    } else {
        email.split('@').next().unwrap_or("user")
    };
    let mapped: String = seed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // validate_name forbids leading '.'/'-'; strip those and cap the length.
    let trimmed = mapped.trim_matches(|c| c == '-' || c == '.');
    let slug: String = trimmed.chars().take(80).collect();
    if slug.is_empty() {
        "user".to_string()
    } else {
        slug.to_ascii_lowercase()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the exact callback URL used as `redirect_uri` on both legs. Honors
/// `RITHUB_OAUTH_REDIRECT_BASE` (e.g. `https://rithub.example.com`); otherwise
/// derives scheme + host from the request (respecting `X-Forwarded-Proto`).
fn callback_uri(headers: &HeaderMap, provider: &str) -> String {
    let base = std::env::var("RITHUB_OAUTH_REDIRECT_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let host = headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("127.0.0.1:8080");
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("http");
            format!("{scheme}://{host}")
        });
    format!(
        "{}/login/oauth/{}/callback",
        base.trim_end_matches('/'),
        provider
    )
}

/// A high-entropy opaque token (used for CSRF state and PKCE verifier).
fn random_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Produce a PKCE `(verifier, challenge)` pair using the S256 method.
fn pkce_pair() -> (String, String) {
    let verifier = random_token();
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
    (verifier, challenge)
}

/// Read + decode the transient OAuth cookie into `(provider, state, verifier)`.
fn read_oauth_cookie(headers: &HeaderMap) -> Option<(String, String, String)> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let encoded = raw.split(';').map(str::trim).find_map(|kv| {
        kv.strip_prefix(&format!("{OAUTH_COOKIE}="))
            .map(str::to_string)
    })?;
    let decoded = STANDARD.decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let mut parts = text.splitn(3, ':');
    let provider = parts.next()?.to_string();
    let state = parts.next()?.to_string();
    let verifier = parts.next()?.to_string();
    Some((provider, state, verifier))
}

/// A short value used for `Option<&str>` when a string may be empty.
fn nonempty(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Render a user-facing error page while also clearing the transient cookie.
fn clear_and_fail(status: StatusCode, message: &str) -> Response {
    let clear = format!("{OAUTH_COOKIE}=; Path=/; HttpOnly; Max-Age=0");
    let page = web::error_page(&None, status, message);
    (AppendHeaders([(header::SET_COOKIE, clear)]), page).into_response()
}

/// Map an [`OauthError`] to a friendly page (config errors are the common case).
fn provider_error_page(provider: &str, error: &OauthError) -> Response {
    let (status, message) = match error {
        OauthError::UnknownProvider(_) => (
            StatusCode::NOT_FOUND,
            "Unknown sign-in provider.".to_string(),
        ),
        OauthError::NotConfigured(name) => (
            StatusCode::NOT_FOUND,
            format!("Sign-in with {name} is not enabled on this server."),
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "The sign-in provider is unavailable.".to_string(),
        ),
    };
    tracing::info!(provider, error = %error, "oauth provider unavailable");
    web::error_page(&None, status, &message)
}

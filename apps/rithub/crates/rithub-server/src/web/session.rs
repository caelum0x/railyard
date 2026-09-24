//! Browser sessions and the sign-in / sign-up / sign-out pages.
//!
//! ## Session model
//! A session is a dedicated, **server-revocable** token row (scope
//! [`crate::authz::SESSION_SCOPE`], 30-day expiry) — never the user's PAT. The
//! `rithub_session` cookie carries only that opaque random secret (the database
//! stores its SHA-256), is `HttpOnly; SameSite=Lax`, gets `Secure` when the
//! public URL is HTTPS, and has an explicit `Max-Age`. Signing out revokes the
//! row, so a copied cookie stops working immediately. Session tokens are
//! rejected by the git transport and the API's bearer/basic paths (they lack
//! repo scopes), so a stolen cookie cannot be replayed outside the browser.
//!
//! ## Sign-in
//! - Username + personal access token (validated, then exchanged for a fresh
//!   session — the PAT itself is never stored in the cookie), or
//! - any configured OAuth/SSO provider (`/login/oauth/{provider}`).
//!
//! ## Sign-up
//! Self-service when `RITHUB_OPEN_SIGNUP` is on (default): creates the account
//! and its personal organization, shows a first access token **once** (needed
//! for git over HTTPS and future sign-ins), and signs the user in.

use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{generate_token, hash_token, Store, User};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

use super::{error_page, esc, page, safe_next};
use crate::authz::SESSION_SCOPE;
use crate::error::AppError;
use crate::state::AppState;

const SESSION_COOKIE: &str = "rithub_session";
const SESSION_DAYS: i64 = 30;

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(&prefix).map(str::to_string))
        .filter(|v| !v.is_empty())
}

/// Whether cookies should carry `Secure` (HTTPS deployments).
fn cookie_secure() -> bool {
    if let Ok(v) = std::env::var("RITHUB_COOKIE_SECURE") {
        return matches!(v.trim(), "1" | "true" | "yes");
    }
    std::env::var("RITHUB_PUBLIC_URL")
        .map(|u| u.trim().starts_with("https://"))
        .unwrap_or(false)
}

fn session_cookie(value: &str, max_age: i64) -> String {
    let secure = if cookie_secure() { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

/// Mint a server-side session for `user` and return its `Set-Cookie` value.
pub(crate) async fn start_session_cookie(state: &AppState, user: &User) -> Result<String, AppError> {
    let secret = generate_token();
    let expires_at = OffsetDateTime::now_utc() + Duration::days(SESSION_DAYS);
    state
        .store
        .create_token(
            user.id,
            "web-session",
            &hash_token(&secret),
            &[SESSION_SCOPE.to_string()],
            Some(expires_at),
        )
        .await?;
    Ok(session_cookie(&secret, SESSION_DAYS * 86_400))
}

/// Validate the session cookie, returning the signed-in user.
pub async fn session_user(state: &AppState, headers: &HeaderMap) -> Option<User> {
    let secret = cookie_value(headers, SESSION_COOKIE)?;
    match state.store.find_user_by_token(&hash_token(&secret)).await {
        Ok(Some((user, token))) if token.scopes.iter().any(|s| s == SESSION_SCOPE) => Some(user),
        _ => None,
    }
}

/// Revoke the current session (if any).
async fn end_session(state: &AppState, headers: &HeaderMap) {
    let Some(secret) = cookie_value(headers, SESSION_COOKIE) else {
        return;
    };
    if let Ok(Some((user, token))) = state.store.find_user_by_token(&hash_token(&secret)).await {
        if token.scopes.iter().any(|s| s == SESSION_SCOPE) {
            if let Err(e) = state.store.revoke_token(user.id, token.id).await {
                tracing::warn!(error = %e, "revoking web session failed");
            }
        }
    }
}

#[derive(Deserialize, Default)]
pub struct NextQuery {
    #[serde(default)]
    next: Option<String>,
}

fn oauth_buttons() -> String {
    let providers: Vec<(&str, &str)> = [("github", "GitHub"), ("google", "Google")]
        .into_iter()
        .filter(|(slug, _)| crate::oauth::is_enabled(slug))
        .collect();
    if providers.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<hr><p class=\"muted\">Or continue with a single-sign-on provider:</p><div class=\"row\">",
    );
    for (slug, label) in providers {
        out.push_str(&format!(
            "<a class=\"btn\" href=\"/login/oauth/{slug}\" hx-boost=\"false\">Continue with {label}</a>",
        ));
    }
    out.push_str("</div>");
    out
}

fn login_body(next: &str, error: Option<&str>) -> String {
    let err = error
        .map(|e| format!("<div class=\"notice bad-notice\" role=\"alert\">{}</div>", esc(e)))
        .unwrap_or_default();
    let signup = if crate::authz::open_signup() {
        "<p class=\"muted\">New here? <a href=\"/signup\">Create an account</a>.</p>"
    } else {
        "<p class=\"muted\">Accounts are created by a site administrator.</p>"
    };
    format!(
        "<div class=\"card\" style=\"max-width:440px;margin:40px auto\">\
         <h1>Sign in to Rithub</h1>{err}\
         <form method=\"post\" action=\"/login\">\
         <input type=\"hidden\" name=\"next\" value=\"{next}\">\
         <label for=\"username\">Username</label>\
         <input id=\"username\" name=\"username\" autocomplete=\"username\" required>\
         <label for=\"token\">Personal access token</label>\
         <input id=\"token\" name=\"token\" type=\"password\" autocomplete=\"current-password\" required>\
         <p class=\"hint\">The token you received at sign-up, or a full-access / admin-scoped token from Settings \u{2192} Tokens.</p>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Sign in</button></p>\
         </form>{oauth}{signup}</div>",
        next = esc(next),
        oauth = oauth_buttons(),
    )
}

/// GET /login
pub async fn login_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = safe_next(q.next.as_deref());
    if session.is_some() {
        return Redirect::to(&next).into_response();
    }
    page("Sign in", &session, &login_body(&next, None)).into_response()
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    token: String,
    #[serde(default)]
    next: Option<String>,
}

/// POST /login — validate a PAT, then open a fresh server-side session.
pub async fn login_submit(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    let next = safe_next(form.next.as_deref());
    let found = state
        .store
        .find_user_by_token(&hash_token(form.token.trim()))
        .await;
    let user = match found {
        Ok(Some((user, token)))
            if user.username == form.username.trim()
                && !token.scopes.iter().any(|s| s == SESSION_SCOPE) =>
        {
            // A browser session has full account access, so only a full-access
            // token (no scope list, or `admin`) may open one — a narrowly
            // scoped token must not be escalated through sign-in.
            if !(token.scopes.is_empty() || token.scopes.iter().any(|s| s == "admin")) {
                return (
                    StatusCode::FORBIDDEN,
                    page(
                        "Sign in",
                        &None,
                        &login_body(
                            &next,
                            Some("That token is scoped; sign in with a full-access token or one with the admin scope."),
                        ),
                    ),
                )
                    .into_response();
            }
            user
        }
        Ok(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                page("Sign in", &None, &login_body(&next, Some("Invalid username or token."))),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "login lookup failed");
            return error_page(&None, StatusCode::INTERNAL_SERVER_ERROR, "sign-in is unavailable");
        }
    };
    match start_session_cookie(&state, &user).await {
        Ok(cookie) => {
            crate::audit::record(&state, None, Some(user.id), &user.username, "auth.login", Some("token"), None)
                .await;
            ([(header::SET_COOKIE, cookie)], Redirect::to(&next)).into_response()
        }
        Err(e) => super::app_error_page(&None, e),
    }
}

/// GET /logout — a confirmation form (sign-out itself is a POST).
pub async fn logout_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    if session.is_none() {
        return Redirect::to("/").into_response();
    }
    let body = "<div class=\"card\" style=\"max-width:440px;margin:40px auto\">\
                <h1>Sign out</h1><p class=\"muted\">End this browser session?</p>\
                <form method=\"post\" action=\"/logout\">\
                <button class=\"btn danger\" type=\"submit\">Sign out</button></form></div>";
    page("Sign out", &session, body).into_response()
}

/// POST /logout — revoke the session server-side and clear the cookie.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    end_session(&state, &headers).await;
    ([(header::SET_COOKIE, session_cookie("", 0))], Redirect::to("/")).into_response()
}

fn signup_body(error: Option<&str>, username: &str, email: &str) -> String {
    let err = error
        .map(|e| format!("<div class=\"notice bad-notice\" role=\"alert\">{}</div>", esc(e)))
        .unwrap_or_default();
    format!(
        "<div class=\"card\" style=\"max-width:440px;margin:40px auto\">\
         <h1>Create your account</h1>{err}\
         <form method=\"post\" action=\"/signup\">\
         <label for=\"su-username\">Username</label>\
         <input id=\"su-username\" name=\"username\" value=\"{u}\" autocomplete=\"username\" required \
                pattern=\"[A-Za-z0-9_][A-Za-z0-9_.\\-]*\" maxlength=\"100\">\
         <p class=\"hint\">Letters, digits, <code>-</code>, <code>_</code>, <code>.</code>. \
         Also becomes your personal organization.</p>\
         <label for=\"su-email\">Email</label>\
         <input id=\"su-email\" name=\"email\" type=\"email\" value=\"{e}\" autocomplete=\"email\" required>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Create account</button></p>\
         </form>{oauth}\
         <p class=\"muted\">Already have an account? <a href=\"/login\">Sign in</a>.</p></div>",
        u = esc(username),
        e = esc(email),
        oauth = oauth_buttons(),
    )
}

/// GET /signup
pub async fn signup_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    if session.is_some() {
        return Redirect::to("/").into_response();
    }
    if !crate::authz::open_signup() {
        return error_page(
            &session,
            StatusCode::FORBIDDEN,
            "self-service sign-up is disabled on this server; ask an administrator for an account",
        );
    }
    page("Sign up", &session, &signup_body(None, "", "")).into_response()
}

#[derive(Deserialize)]
pub struct SignupForm {
    username: String,
    email: String,
}

/// POST /signup — create the account + personal org, show the first token once,
/// and sign the new user in.
pub async fn signup_submit(State(state): State<AppState>, Form(form): Form<SignupForm>) -> Response {
    if !crate::authz::open_signup() {
        return error_page(&None, StatusCode::FORBIDDEN, "self-service sign-up is disabled");
    }
    let username = form.username.trim().to_string();
    let email = form.email.trim().to_string();
    let user = match crate::api::provision_user(&state, &username, &email).await {
        Ok(user) => user,
        Err(e) => {
            let msg = match e.status() {
                StatusCode::CONFLICT => "That username or email is already taken.".to_string(),
                _ => e.client_message(),
            };
            return (
                e.status(),
                page("Sign up", &None, &signup_body(Some(&msg), &username, &email)),
            )
                .into_response();
        }
    };
    let token = match crate::api::mint_token(&state, &user, "initial", &[], None).await {
        Ok((token, _)) => token,
        Err(e) => return super::app_error_page(&None, e),
    };
    let cookie = match start_session_cookie(&state, &user).await {
        Ok(c) => c,
        Err(e) => return super::app_error_page(&None, e),
    };
    let session = Some(user.clone());
    let body = format!(
        "<div class=\"card\" style=\"max-width:640px;margin:24px auto\">\
         {notice}<h1>Save your access token</h1>\
         <p>This token signs you in and authenticates <code>git</code> over HTTPS. \
         It is shown <strong>only once</strong> \u{2014} store it in a password manager.</p>\
         <div class=\"secret mono\">{token}</div>\
         <p class=\"hint\">Use it as the password when git asks for credentials, with username \
         <code>{u}</code>. You can create more tokens or add an SSH key in Settings.</p>\
         <div class=\"row\" style=\"margin-top:12px\">\
         <a class=\"btn primary\" href=\"/new\">Create a repository</a>\
         <a class=\"btn\" href=\"/settings/keys\">Add an SSH key</a>\
         <a class=\"btn\" href=\"/\">Go to dashboard</a></div></div>",
        notice = super::notice_html(Some("welcome")),
        token = esc(&token),
        u = esc(&user.username),
    );
    (
        [(header::SET_COOKIE, cookie)],
        page("Welcome", &session, &body),
    )
        .into_response()
}

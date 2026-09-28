//! OAuth 2.1 endpoints for MCP clients and the CLI, plus the three small
//! human pages: consent, device approval and per-tool send/spend consent.
//! Sign-in is Scribase's: the pages read the `scribase_session` cookie.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use serde_json::{json, Value};
use switchyard_auth::oauth::{AuthorizeParams, OAuthError, TokenRequest};
use switchyard_auth::{hash_token, scopes, User};
use switchyard_store::Project;

use crate::http_util::html_escape as esc;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "scribase_session";

fn oauth_error(e: OAuthError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
    (status, [("cache-control", "no-store")], Json(json!({ "error": e.error, "error_description": e.error_description }))).into_response()
}

pub async fn as_metadata(State(st): State<AppState>) -> Response {
    Json(st.oauth.authorization_server_metadata()).into_response()
}

pub async fn pr_metadata_mcp(State(st): State<AppState>) -> Response {
    Json(st.oauth.protected_resource_metadata(&st.oauth.cfg.mcp_resource())).into_response()
}

pub async fn pr_metadata_api(State(st): State<AppState>) -> Response {
    Json(st.oauth.protected_resource_metadata(&st.oauth.cfg.issuer)).into_response()
}

pub async fn jwks(State(st): State<AppState>) -> Response {
    Json(st.signer.jwks()).into_response()
}

pub async fn register(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return oauth_error(OAuthError::new(400, "invalid_client_metadata", e.to_string())),
    };
    match st.oauth.register(&v).await {
        Ok(c) => (
            StatusCode::CREATED,
            Json(json!({
                "client_id": c.client_id,
                "client_id_issued_at": c.created_at.timestamp(),
                "redirect_uris": c.redirect_uris,
                "client_name": c.metadata.get("client_name"),
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })),
        )
            .into_response(),
        Err(e) => oauth_error(e),
    }
}

/// Form-encoded (standard) or JSON token requests.
pub async fn token(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let is_json = headers.get("content-type").and_then(|v| v.to_str().ok()).map(|c| c.contains("json")).unwrap_or(false);
    let req: Result<TokenRequest, String> = if is_json {
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    } else {
        serde_urlencoded_like(&body)
    };
    let req = match req {
        Ok(r) => r,
        Err(e) => return oauth_error(OAuthError::new(400, "invalid_request", e)),
    };
    match st.oauth.token(&req).await {
        Ok(t) => ([("cache-control", "no-store")], Json(t)).into_response(),
        Err(e) => oauth_error(e),
    }
}

fn serde_urlencoded_like(body: &[u8]) -> Result<TokenRequest, String> {
    let map: HashMap<String, String> = url::form_urlencoded::parse(body).into_owned().collect();
    serde_json::from_value(serde_json::to_value(map).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

pub async fn device_code(State(st): State<AppState>, body: Bytes) -> Response {
    let map: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let Some(client_id) = map.get("client_id") else {
        return oauth_error(OAuthError::new(400, "invalid_request", "client_id is required"));
    };
    match st.oauth.device_start(client_id, map.get("scope").map(String::as_str)).await {
        Ok((d, _)) => ([("cache-control", "no-store")], Json(d)).into_response(),
        Err(e) => oauth_error(e),
    }
}

// ----------------------------------------------------------- human pages

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all("cookie").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title>
<style>body{{font:16px/1.5 system-ui,-apple-system,sans-serif;background:#f4f5f7;color:#1c2330;margin:0;display:flex;min-height:100vh;align-items:center;justify-content:center}}
main{{background:#fff;border-radius:16px;box-shadow:0 8px 30px rgba(20,30,50,.08);padding:32px;max-width:460px;width:100%}}
h1{{font-size:20px;margin:0 0 12px}}p{{color:#4a5568}}label{{display:block;margin:8px 0}}select,input[type=text]{{width:100%;padding:10px;border:1px solid #d5dae1;border-radius:10px;font:inherit}}
.row{{display:flex;gap:12px;margin-top:20px}}button{{flex:1;padding:12px;border-radius:10px;border:0;font:inherit;cursor:pointer}}
.primary{{background:#2f5bea;color:#fff}}.secondary{{background:#edf0f5;color:#1c2330}}code{{background:#edf0f5;padding:2px 6px;border-radius:6px}}.muted{{font-size:13px;color:#718096}}</style>
</head><body><main>{body}</main></body></html>",
        t = esc(title)
    ))
}

fn csrf(st: &AppState, session: &str, context: &str) -> String {
    hash_token(&format!("{}|{session}|{context}", st.settings.form_secret))
}

async fn current_user(st: &AppState, headers: &HeaderMap) -> Option<(String, User)> {
    let s = cookie(headers, SESSION_COOKIE)?;
    st.identity.user_from_session(&s).await.ok().map(|u| (s, u))
}

/// Projects the user can grant, as `(org_id, project)`.
async fn memberships(st: &AppState, user: &User) -> Vec<(String, String, Project)> {
    let mut out = Vec::new();
    for org in st.store.list_orgs().await.unwrap_or_default() {
        if let Ok(Some(m)) = st.store.get_member(&org.id, &user.id).await {
            if m.role == "viewer" {
                continue;
            }
            for p in st.store.list_projects(&org.id).await.unwrap_or_default() {
                out.push((org.id.clone(), org.name.clone(), p));
            }
        }
    }
    out
}

fn sign_in_page(st: &AppState, return_to: &str) -> Response {
    if let Some(url) = st.identity.login_url(return_to) {
        return Redirect::to(&url).into_response();
    }
    page(
        "Sign in",
        &format!(
            "<h1>Sign in to continue</h1><p>Paste your Scribase session to continue.</p>
<form method=\"post\" action=\"/oauth/session\"><input type=\"hidden\" name=\"return_to\" value=\"{}\"><label>Session<input type=\"text\" name=\"session\" autocomplete=\"off\"></label>
<div class=\"row\"><button class=\"primary\" type=\"submit\">Continue</button></div></form>",
            esc(return_to)
        ),
    )
    .into_response()
}

pub async fn set_session(State(st): State<AppState>, Form(f): Form<HashMap<String, String>>) -> Response {
    let session = f.get("session").cloned().unwrap_or_default();
    let return_to = f.get("return_to").cloned().unwrap_or_else(|| "/".into());
    if !return_to.starts_with('/') || return_to.starts_with("//") {
        return (StatusCode::BAD_REQUEST, "bad return_to").into_response();
    }
    if st.identity.user_from_session(&session).await.is_err() {
        return page("Sign in", "<h1>That session is not valid</h1><p>Sign in to Scribase again and retry.</p>").into_response();
    }
    let secure = if st.settings.public_url.starts_with("https://") { "; Secure" } else { "" };
    ([("set-cookie", format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age=3600{secure}"))], Redirect::to(&return_to)).into_response()
}

fn project_select(ms: &[(String, String, Project)]) -> String {
    let opts: String = ms
        .iter()
        .map(|(org, org_name, p)| format!("<option value=\"{}|{}\">{} / {}</option>", esc(org), esc(&p.id), esc(org_name), esc(&p.name)))
        .collect();
    format!("<label>Project<select name=\"project\">{opts}</select></label>")
}

fn scope_boxes(requested: &[String]) -> String {
    requested
        .iter()
        .map(|s| {
            let checked = if scopes::DEFAULT_CONSENT.contains(&s.as_str()) { " checked" } else { "" };
            let hint = match s.as_str() {
                "tools:read" => "search and describe tools",
                "tools:call" => "run read-only tools",
                "tools:write" => "run tools that change data",
                "tools:send" => "send email and messages",
                "tools:spend" => "start paid jobs (renders)",
                _ => "manage keys, credentials and caps",
            };
            format!("<label><input type=\"checkbox\" name=\"scope_{s}\" value=\"1\"{checked}> <code>{s}</code> {hint}</label>")
        })
        .collect()
}

fn granted_from_form(f: &HashMap<String, String>, requested: &[String]) -> Vec<String> {
    requested.iter().filter(|s| f.contains_key(&format!("scope_{s}"))).cloned().collect()
}

fn parse_project(f: &HashMap<String, String>) -> Option<(String, String)> {
    f.get("project").and_then(|v| v.split_once('|')).map(|(a, b)| (a.to_string(), b.to_string()))
}

pub async fn authorize_page(State(st): State<AppState>, headers: HeaderMap, uri: axum::http::Uri, Query(p): Query<AuthorizeParams>) -> Response {
    let (client, requested, _) = match st.oauth.validate_authorize(&p).await {
        Ok(v) => v,
        Err(e) => return page("Authorization error", &format!("<h1>This sign-in link is not valid</h1><p>{}</p>", esc(&e.error_description))).into_response(),
    };
    let Some((session, user)) = current_user(&st, &headers).await else {
        return sign_in_page(&st, &uri.to_string());
    };
    let ms = memberships(&st, &user).await;
    if ms.is_empty() {
        return page("No project", "<h1>No project yet</h1><p>Your account is not a member of any Switchyard org. Ask an owner to add you.</p>").into_response();
    }
    let client_name = client.metadata.get("client_name").and_then(Value::as_str).unwrap_or(&client.client_id).to_string();
    let hidden: String = [
        ("response_type", Some(p.response_type.clone())),
        ("client_id", Some(p.client_id.clone())),
        ("redirect_uri", Some(p.redirect_uri.clone())),
        ("scope", p.scope.clone()),
        ("state", p.state.clone()),
        ("code_challenge", p.code_challenge.clone()),
        ("code_challenge_method", p.code_challenge_method.clone()),
        ("resource", p.resource.clone()),
        ("csrf", Some(csrf(&st, &session, &format!("{}|{}", p.client_id, p.code_challenge.clone().unwrap_or_default())))),
    ]
    .iter()
    .filter_map(|(k, v)| v.as_ref().map(|v| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(v))))
    .collect();
    page(
        "Allow access",
        &format!(
            "<h1>Allow {} to use your tools?</h1><p>Signed in as {}. Pick the project whose balance and caps apply.</p>
<form method=\"post\" action=\"/oauth/authorize\">{hidden}{}{}
<p class=\"muted\">Send and spend tools also ask once per tool before their first use.</p>
<div class=\"row\"><button class=\"secondary\" name=\"decision\" value=\"deny\">Deny</button><button class=\"primary\" name=\"decision\" value=\"approve\">Allow</button></div></form>",
            esc(&client_name),
            esc(&user.email),
            project_select(&ms),
            scope_boxes(&requested)
        ),
    )
    .into_response()
}

pub async fn authorize_submit(State(st): State<AppState>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>) -> Response {
    let p: AuthorizeParams = match serde_json::from_value(json!(f)) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let (_, requested, _) = match st.oauth.validate_authorize(&p).await {
        Ok(v) => v,
        Err(e) => return page("Authorization error", &format!("<h1>Request not valid</h1><p>{}</p>", esc(&e.error_description))).into_response(),
    };
    let Some((session, user)) = current_user(&st, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let expected = csrf(&st, &session, &format!("{}|{}", p.client_id, p.code_challenge.clone().unwrap_or_default()));
    if f.get("csrf") != Some(&expected) {
        return (StatusCode::FORBIDDEN, "invalid form token").into_response();
    }
    if f.get("decision").map(String::as_str) != Some("approve") {
        return Redirect::to(&st.oauth.denied_redirect(&p)).into_response();
    }
    let Some((org, project)) = parse_project(&f) else {
        return (StatusCode::BAD_REQUEST, "pick a project").into_response();
    };
    if !memberships(&st, &user).await.iter().any(|(o, _, pr)| o == &org && pr.id == project) {
        return (StatusCode::FORBIDDEN, "not a member of that project").into_response();
    }
    let granted = granted_from_form(&f, &requested);
    match st.oauth.issue_code(&p, &user.id, &org, &project, granted).await {
        Ok(url) => Redirect::to(&url).into_response(),
        Err(e) => oauth_error(e),
    }
}

pub async fn device_page(State(st): State<AppState>, headers: HeaderMap, uri: axum::http::Uri, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some((session, user)) = current_user(&st, &headers).await else {
        return sign_in_page(&st, &uri.to_string());
    };
    let code = q.get("user_code").cloned().unwrap_or_default();
    let requested = match st.oauth.device_pending(&code).await {
        Ok(d) => d.scopes,
        Err(_) => {
            return page(
                "Connect a device",
                "<h1>Connect a device</h1><p>Enter the code shown in your terminal.</p><form method=\"get\" action=\"/device\"><label>Code<input type=\"text\" name=\"user_code\" placeholder=\"ABCD-EFGH\"></label><div class=\"row\"><button class=\"primary\">Continue</button></div></form>",
            )
            .into_response()
        }
    };
    let ms = memberships(&st, &user).await;
    if ms.is_empty() {
        return page("No project", "<h1>No project yet</h1><p>Ask an owner to add you to an org.</p>").into_response();
    }
    page(
        "Connect the CLI",
        &format!(
            "<h1>Connect the Switchyard CLI</h1><p>Confirm the code <code>{}</code> matches your terminal.</p>
<form method=\"post\" action=\"/device\"><input type=\"hidden\" name=\"user_code\" value=\"{}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">{}{}
<div class=\"row\"><button class=\"secondary\" name=\"decision\" value=\"deny\">Deny</button><button class=\"primary\" name=\"decision\" value=\"approve\">Connect</button></div></form>",
            esc(&code),
            esc(&code),
            csrf(&st, &session, &code.to_uppercase()),
            project_select(&ms),
            scope_boxes(&requested)
        ),
    )
    .into_response()
}

pub async fn device_submit(State(st): State<AppState>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>) -> Response {
    let Some((session, user)) = current_user(&st, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let code = f.get("user_code").cloned().unwrap_or_default().to_uppercase();
    if f.get("csrf") != Some(&csrf(&st, &session, &code)) {
        return (StatusCode::FORBIDDEN, "invalid form token").into_response();
    }
    let requested = match st.oauth.device_pending(&code).await {
        Ok(d) => d.scopes,
        Err(e) => return page("Code expired", &format!("<h1>Code not valid</h1><p>{}</p>", esc(&e.error_description))).into_response(),
    };
    let approve = f.get("decision").map(String::as_str) == Some("approve");
    let (org, project) = parse_project(&f).unwrap_or_default();
    if approve && !memberships(&st, &user).await.iter().any(|(o, _, pr)| o == &org && pr.id == project) {
        return (StatusCode::FORBIDDEN, "not a member of that project").into_response();
    }
    let granted = granted_from_form(&f, &requested);
    match st.oauth.device_decide(&code, &user.id, &org, &project, granted, approve).await {
        Ok(()) if approve => page("Connected", "<h1>Connected</h1><p>Return to your terminal; the CLI is signed in.</p>").into_response(),
        Ok(()) => page("Denied", "<h1>Request denied</h1><p>The CLI was not connected.</p>").into_response(),
        Err(e) => page("Error", &format!("<h1>Could not complete</h1><p>{}</p>", esc(&e.error_description))).into_response(),
    }
}

/// One-time consent for a send/spend tool used through an OAuth grant.
pub async fn tool_consent_page(State(st): State<AppState>, headers: HeaderMap, uri: axum::http::Uri, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some((session, user)) = current_user(&st, &headers).await else {
        return sign_in_page(&st, &uri.to_string());
    };
    let (tool, grant, org) = (q.get("tool").cloned().unwrap_or_default(), q.get("grant").cloned().unwrap_or_default(), q.get("org").cloned().unwrap_or_default());
    let Some(t) = st.catalog.tool(&tool) else {
        return page("Unknown tool", "<h1>Unknown tool</h1>").into_response();
    };
    if st.store.get_member(&org, &user.id).await.ok().flatten().is_none() {
        return page("Not allowed", "<h1>Not a member of this org</h1>").into_response();
    }
    page(
        "Approve tool",
        &format!(
            "<h1>Allow {} ?</h1><p>{}</p><p>Price: <code>{} micro-USD</code> ({:?}). Side effect: <code>{}</code>.</p>
<form method=\"post\" action=\"/oauth/consent\"><input type=\"hidden\" name=\"tool\" value=\"{}\"><input type=\"hidden\" name=\"grant\" value=\"{}\"><input type=\"hidden\" name=\"org\" value=\"{}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">
<div class=\"row\"><button class=\"primary\" name=\"decision\" value=\"approve\">Allow</button></div></form>",
            esc(&t.title),
            esc(&t.description),
            t.price.micros,
            t.price.basis,
            t.side_effect.as_str(),
            esc(&tool),
            esc(&grant),
            esc(&org),
            csrf(&st, &session, &format!("{tool}|{grant}"))
        ),
    )
    .into_response()
}

pub async fn tool_consent_submit(State(st): State<AppState>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>) -> Response {
    let Some((session, user)) = current_user(&st, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "sign in first").into_response();
    };
    let (tool, grant, org) = (f.get("tool").cloned().unwrap_or_default(), f.get("grant").cloned().unwrap_or_default(), f.get("org").cloned().unwrap_or_default());
    if f.get("csrf") != Some(&csrf(&st, &session, &format!("{tool}|{grant}"))) {
        return (StatusCode::FORBIDDEN, "invalid form token").into_response();
    }
    if st.catalog.tool(&tool).is_none() || st.store.get_member(&org, &user.id).await.ok().flatten().is_none() {
        return (StatusCode::FORBIDDEN, "not allowed").into_response();
    }
    match st.store.add_consent(&grant, &tool).await {
        Ok(()) => page("Approved", "<h1>Approved</h1><p>Return to your agent and retry the call.</p>").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

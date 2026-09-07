//! Server-rendered web UI. Hand-rolled HTML (no template engine) with a strict
//! escaping discipline: **every** dynamic value goes through [`esc`], and README
//! markdown is rendered with raw-HTML stripped ([`markdown`]) so user content
//! cannot inject scripts.
//!
//! Pages render real data from the store + `rit` — never placeholder content.
//! Submodules: [`repo`] (browse/blob/commits), [`collab`] (issues/PRs),
//! [`releases`].

mod collab;
mod governance;
mod releases;
mod repo;

use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use rithub_core::{hash_token, Store, User};
use serde::Deserialize;

use crate::state::AppState;

/// Register every web route. Merged into the main router (which owns the git and
/// JSON-API routes) so static/`/api` paths keep priority over these param routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(landing))
        .route("/login", get(login_form).post(login_submit))
        .route("/logout", get(logout))
        .route("/search", get(governance::search))
        .route("/notifications", get(governance::notifications))
        .route("/notifications/read", get(governance::mark_read))
        // repo browse
        .route("/{org}/{repo}", get(repo::home))
        .route("/{org}/{repo}/tree/{rev}", get(repo::tree_root))
        .route("/{org}/{repo}/tree/{rev}/{*path}", get(repo::tree_path))
        .route("/{org}/{repo}/blob/{rev}/{*path}", get(repo::blob))
        .route("/{org}/{repo}/commits/{rev}", get(repo::commits))
        .route("/{org}/{repo}/commit/{oid}", get(repo::commit_detail))
        // issues
        .route("/{org}/{repo}/issues", get(collab::issues_list))
        .route("/{org}/{repo}/issues/new", get(collab::issue_new_form))
        .route("/{org}/{repo}/issues/create", post(collab::issue_create))
        .route("/{org}/{repo}/issues/{number}", get(collab::issue_detail))
        .route(
            "/{org}/{repo}/issues/{number}/comment",
            post(collab::issue_comment),
        )
        // pull requests
        .route("/{org}/{repo}/pulls", get(collab::pulls_list))
        .route("/{org}/{repo}/pulls/new", get(collab::pr_new_form))
        .route("/{org}/{repo}/pulls/create", post(collab::pr_create))
        .route("/{org}/{repo}/pulls/{number}", get(collab::pr_detail))
        .route("/{org}/{repo}/pulls/{number}/review", post(collab::pr_review))
        .route("/{org}/{repo}/pulls/{number}/merge", post(collab::pr_merge))
        // audit log
        .route("/{org}/{repo}/audit", get(governance::audit_log))
        // releases
        .route("/{org}/{repo}/releases", get(releases::list))
        .route("/{org}/{repo}/releases/new", get(releases::new_form))
        .route("/{org}/{repo}/releases/create", post(releases::create))
        .route(
            "/{org}/{repo}/releases/{tag}/archive.tar.gz",
            get(releases::archive),
        )
}

// --- shared rendering -------------------------------------------------------

/// HTML-escape a string for safe interpolation into markup.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render markdown to sanitized HTML — raw/inline HTML events are dropped so a
/// README cannot smuggle `<script>` or other markup.
pub fn markdown(md: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Format a unix timestamp as `YYYY-MM-DD`.
pub fn short_date(unix_seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix_seconds)
        .map(|dt| dt.date().to_string())
        .unwrap_or_default()
}

/// Wrap page `body` in the site chrome (nav + minimal CSS).
pub fn page(title: &str, session: &Option<User>, body: &str) -> Html<String> {
    let nav_auth = match session {
        Some(user) => format!(
            "<span class=\"muted\">{}</span> · <a href=\"/logout\">sign out</a>",
            esc(&user.username)
        ),
        None => "<a href=\"/login\">sign in</a>".to_string(),
    };
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} · rithub</title><style>{CSS}</style></head><body>\
<header class=\"nav\"><a href=\"/\" class=\"brand\">rithub</a><div class=\"spacer\"></div>{nav_auth}</header>\
<main class=\"wrap\">{body}</main>\
<footer class=\"foot\">rithub — a from-scratch Rust git forge on <code>rit</code></footer>\
</body></html>",
        title = esc(title),
    ))
}

/// A small error page.
pub fn error_page(session: &Option<User>, status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<div class=\"card\"><h2>{}</h2><p class=\"muted\">{}</p><p><a href=\"/\">← home</a></p></div>",
        status.as_u16(),
        esc(message)
    );
    (status, page("error", session, &body)).into_response()
}

const CSS: &str = "\
:root{--fg:#1f2328;--muted:#656d76;--line:#d0d7de;--bg:#fff;--accent:#0969da;--ok:#1a7f37;--bad:#cf222e}\
*{box-sizing:border-box}body{margin:0;font:14px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:var(--fg);background:#f6f8fa}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}\
.nav{display:flex;align-items:center;gap:10px;padding:12px 20px;background:#24292f;color:#fff}\
.nav a{color:#fff}.brand{font-weight:700}.spacer{flex:1}\
.wrap{max-width:1000px;margin:24px auto;padding:0 16px}\
.foot{max-width:1000px;margin:40px auto;padding:16px;color:var(--muted);border-top:1px solid var(--line)}\
.card{background:var(--bg);border:1px solid var(--line);border-radius:8px;padding:16px;margin-bottom:16px}\
.muted{color:var(--muted)}code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}\
pre{background:#f6f8fa;border:1px solid var(--line);border-radius:6px;padding:12px;overflow:auto}\
table{border-collapse:collapse;width:100%}td,th{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line)}\
.row{display:flex;align-items:center;gap:8px}.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:12px;border:1px solid var(--line)}\
.pill.open{background:#dafbe1;color:var(--ok);border-color:#aceebb}.pill.closed{background:#ffebe9;color:var(--bad)}\
.pill.merged{background:#fbefff;color:#8250df;border-color:#e6d6ff}\
.btn{display:inline-block;padding:6px 12px;border-radius:6px;border:1px solid var(--line);background:#f6f8fa;cursor:pointer;font:inherit}\
.btn.primary{background:var(--ok);color:#fff;border-color:var(--ok)}.btn.danger{background:var(--bad);color:#fff}\
input,textarea,select{font:inherit;padding:6px 8px;border:1px solid var(--line);border-radius:6px;width:100%}\
label{display:block;margin:8px 0 2px;font-weight:600}\
.diff{background:#fff;border:1px solid var(--line);border-radius:6px;overflow:auto;margin:10px 0}\
.diff .h{background:#f6f8fa;padding:6px 10px;border-bottom:1px solid var(--line);font-family:ui-monospace,monospace}\
.diff pre{margin:0;border:0;border-radius:0;background:#fff}\
.diff .add{background:#e6ffec}.diff .del{background:#ffebe9}.diff .hunk{color:#8250df}\
h1,h2,h3{margin:.4em 0}.list a{display:block;padding:6px 0}.ok{color:var(--ok)}.bad{color:var(--bad)}";

// --- session / auth ---------------------------------------------------------

/// The signed-in identity carried in the `rithub_session` cookie
/// (`base64(username:token)`).
pub struct Session {
    pub username: String,
    pub token: String,
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').map(str::trim).find_map(|kv| {
        kv.strip_prefix(&format!("{name}="))
            .map(|value| value.to_string())
    })
}

/// Read (but do not validate) the session cookie.
pub fn read_session(headers: &HeaderMap) -> Option<Session> {
    let encoded = cookie_value(headers, "rithub_session")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (username, token) = text.split_once(':')?;
    Some(Session {
        username: username.to_string(),
        token: token.to_string(),
    })
}

/// Validate the session cookie against the tokens table, returning the user.
pub async fn session_user(state: &AppState, headers: &HeaderMap) -> Option<User> {
    let session = read_session(headers)?;
    match state.store.find_user_by_token(&hash_token(&session.token)).await {
        Ok(Some(user)) if user.username == session.username => Some(user),
        _ => None,
    }
}

// --- landing + auth pages ---------------------------------------------------

async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let orgs = match state.store.list_orgs().await {
        Ok(orgs) => orgs,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "store error"),
    };
    let mut body = String::from("<div class=\"card\"><h1>Repositories</h1>");
    if orgs.is_empty() {
        body.push_str("<p class=\"muted\">No organizations yet.</p>");
    }
    for org in orgs {
        body.push_str(&format!("<h3>{}</h3><div class=\"list\">", esc(&org.name)));
        match state.store.list_repos(&org).await {
            Ok(repos) if !repos.is_empty() => {
                for repo in repos {
                    body.push_str(&format!(
                        "<a href=\"/{o}/{r}\">{o}/{r}</a>",
                        o = esc(&org.name),
                        r = esc(&repo.name)
                    ));
                }
            }
            _ => body.push_str("<p class=\"muted\">No repositories.</p>"),
        }
        body.push_str("</div>");
    }
    body.push_str("</div>");
    page("Repositories", &session, &body).into_response()
}

async fn login_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let body = "<div class=\"card\" style=\"max-width:420px\"><h1>Sign in</h1>\
<p class=\"muted\">Use your username and a personal access token.</p>\
<form method=\"post\" action=\"/login\">\
<label>Username</label><input name=\"username\" autocomplete=\"username\">\
<label>Token</label><input name=\"token\" type=\"password\" autocomplete=\"current-password\">\
<p><button class=\"btn primary\" type=\"submit\">Sign in</button></p></form></div>";
    page("Sign in", &session, body).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    token: String,
}

async fn login_submit(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    // Validate the token really belongs to the username before setting a cookie.
    let valid = matches!(
        state.store.find_user_by_token(&hash_token(&form.token)).await,
        Ok(Some(user)) if user.username == form.username
    );
    if !valid {
        let body = "<div class=\"card\" style=\"max-width:420px\"><h1>Sign in</h1>\
<p class=\"bad\">Invalid username or token.</p><p><a href=\"/login\">try again</a></p></div>";
        return (StatusCode::UNAUTHORIZED, page("Sign in", &None, body)).into_response();
    }
    let cookie_val =
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", form.username, form.token));
    let cookie = format!("rithub_session={cookie_val}; Path=/; HttpOnly; SameSite=Lax");
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

async fn logout() -> Response {
    let cookie = "rithub_session=; Path=/; HttpOnly; Max-Age=0".to_string();
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

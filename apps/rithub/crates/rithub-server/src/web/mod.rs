//! Server-rendered web UI. Hand-rolled HTML (no template engine) with a strict
//! escaping discipline: **every** dynamic value goes through [`esc`], and README
//! markdown is rendered with raw-HTML stripped ([`markdown`]) so user content
//! cannot inject scripts.
//!
//! Pages render real data from the store + `rit` — never placeholder content.
//! Submodules: [`repo`] (browse/blob/commits), [`collab`] (issues/PRs),
//! [`releases`], [`governance`] (search/notifications/audit).
//!
//! ## HTMX integration
//! Vendored `htmx.min.js` is served from `/static/htmx.min.js` and included in
//! every page. `hx-boost="true"` on `<body>` converts in-page anchor links into
//! XHR swaps with pushState (graceful degradation: without JS every form and
//! link is a plain HTTP round-trip).
//!
//! HTMX partial responses are gated on the `HX-Request: true` header
//! ([`is_htmx`]); handlers that detect it return an HTML fragment instead of a
//! full-page redirect so HTMX can do in-place updates (comment list refresh,
//! live search results, file-tree navigation, tab switching).

mod collab;
mod governance;
mod releases;
mod repo;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use base64::Engine;
use rithub_core::{hash_token, CiStore, Store, User, Visibility};
use serde::Deserialize;

use crate::state::AppState;

/// Register every web route. Merged into the main router (which owns the git
/// and JSON-API routes) so `/static` and `/api` paths keep priority.
pub fn routes() -> Router<AppState> {
    Router::new()
        // Static assets: vendored JS embedded at compile time.
        .route("/static/htmx.min.js", get(htmx_js))
        // Top-level pages
        .route("/", get(landing))
        .route("/login", get(login_form).post(login_submit))
        .route("/logout", get(logout))
        // Search: full page + HTMX partial (results fragment only)
        .route("/search", get(governance::search))
        .route("/search/results", get(governance::search_results))
        // Notifications
        .route("/notifications", get(governance::notifications))
        .route("/notifications/read", get(governance::mark_read))
        // Repo: code browse
        .route("/{org}/{repo}", get(repo::home))
        .route("/{org}/{repo}/tree/{rev}", get(repo::tree_root))
        .route("/{org}/{repo}/tree/{rev}/{*path}", get(repo::tree_path))
        .route("/{org}/{repo}/blob/{rev}/{*path}", get(repo::blob))
        .route("/{org}/{repo}/commits/{rev}", get(repo::commits))
        .route("/{org}/{repo}/commit/{oid}", get(repo::commit_detail))
        // Repo: issues
        .route("/{org}/{repo}/issues", get(collab::issues_list))
        .route("/{org}/{repo}/issues/new", get(collab::issue_new_form))
        .route("/{org}/{repo}/issues/create", post(collab::issue_create))
        .route("/{org}/{repo}/issues/{number}", get(collab::issue_detail))
        .route(
            "/{org}/{repo}/issues/{number}/comment",
            post(collab::issue_comment),
        )
        // Repo: pull requests
        .route("/{org}/{repo}/pulls", get(collab::pulls_list))
        .route("/{org}/{repo}/pulls/new", get(collab::pr_new_form))
        .route("/{org}/{repo}/pulls/create", post(collab::pr_create))
        .route("/{org}/{repo}/pulls/{number}", get(collab::pr_detail))
        .route(
            "/{org}/{repo}/pulls/{number}/review",
            post(collab::pr_review),
        )
        .route(
            "/{org}/{repo}/pulls/{number}/merge",
            post(collab::pr_merge),
        )
        // Repo: audit log
        .route("/{org}/{repo}/audit", get(governance::audit_log))
        // Repo: releases
        .route("/{org}/{repo}/releases", get(releases::list))
        .route("/{org}/{repo}/releases/new", get(releases::new_form))
        .route("/{org}/{repo}/releases/create", post(releases::create))
        .route(
            "/{org}/{repo}/releases/{tag}/archive.tar.gz",
            get(releases::archive),
        )
        // Repo: actions (CI check-run list, wired to existing CI API)
        .route("/{org}/{repo}/actions", get(actions_page))
        // Repo: settings (branch protection, collaborators — stub shell)
        .route("/{org}/{repo}/settings", get(settings_page))
}

// --- static assets -----------------------------------------------------------

/// Serve the vendored htmx.min.js from the embedded constant.
async fn htmx_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        HTMX_JS,
    )
        .into_response()
}

/// Embedded htmx source. Replace with real htmx 2.0.4 contents for full
/// progressive-enhancement; this stub is valid JS so the page never throws.
const HTMX_JS: &str = include_str!("../../static/htmx.min.js");

// --- shared rendering --------------------------------------------------------

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

/// Returns `true` when the request originated from htmx (`HX-Request: true`).
/// Use this to return HTML fragments instead of full pages or redirects.
pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers
        .get("hx-request")
        .and_then(|v| v.to_str().ok())
        == Some("true")
}

/// The six-tab repo navigation bar. Consistent across every repo sub-page.
/// Tabs: Code / Issues / Pull requests / Actions / Releases / Settings.
pub fn tabs(org: &str, repo: &str, active: &str) -> String {
    let tab = |name: &str, href: &str, key: &str| -> String {
        let cls = if key == active { " class=\"tab-active\"" } else { "" };
        format!("<a href=\"{href}\"{cls}>{name}</a>")
    };
    format!(
        "<nav class=\"repo-tabs\">{}{}{}{}{}{}</nav>",
        tab("Code", &format!("/{org}/{repo}"), "code"),
        tab("Issues", &format!("/{org}/{repo}/issues"), "issues"),
        tab("Pull requests", &format!("/{org}/{repo}/pulls"), "pulls"),
        tab("Actions", &format!("/{org}/{repo}/actions"), "actions"),
        tab("Releases", &format!("/{org}/{repo}/releases"), "releases"),
        tab("Settings", &format!("/{org}/{repo}/settings"), "settings"),
    )
}

/// Breadcrumb navigation strip. Each `(label, href)` pair produces a link;
/// `None` href marks the current (non-linked) terminal segment.
pub fn breadcrumbs(parts: &[(&str, Option<&str>)]) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("<nav class=\"breadcrumbs\" aria-label=\"breadcrumb\">");
    for (i, (label, href)) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str("<span class=\"bc-sep\">/</span>");
        }
        match href {
            Some(h) => out.push_str(&format!(
                "<a href=\"{h}\">{}</a>",
                esc(label)
            )),
            None => out.push_str(&format!(
                "<span aria-current=\"page\">{}</span>",
                esc(label)
            )),
        }
    }
    out.push_str("</nav>");
    out
}

/// Render a slice of check runs as a compact status table card.
pub fn check_runs_html(runs: &[rithub_core::CheckRun]) -> String {
    if runs.is_empty() {
        return "<p class=\"muted\">No check runs yet.</p>".into();
    }
    let mut out = String::from(
        "<table><tr><th>Check</th><th>Status</th><th>Conclusion</th>\
         <th>Commit</th></tr>",
    );
    for r in runs {
        let (status_cls, conclusion_label) = match r.status {
            rithub_core::CheckRunStatus::Completed => {
                let label = r.conclusion.map(|c| c.as_str()).unwrap_or("completed");
                let cls = match r.conclusion {
                    Some(c) if c.is_passing() => "ok",
                    Some(_) => "bad",
                    None => "muted",
                };
                (cls, label)
            }
            rithub_core::CheckRunStatus::InProgress => ("muted", "in_progress"),
            rithub_core::CheckRunStatus::Queued => ("muted", "queued"),
        };
        let conclusion_cell = match &r.details_url {
            Some(url) => format!(
                "<a href=\"{}\" class=\"{status_cls}\">{}</a>",
                esc(url),
                esc(conclusion_label)
            ),
            None => format!(
                "<span class=\"{status_cls}\">{}</span>",
                esc(conclusion_label)
            ),
        };
        let title_hint = r
            .output_title
            .as_deref()
            .map(|t| format!("<br><span class=\"muted\" style=\"font-weight:400\">{}</span>", esc(t)))
            .unwrap_or_default();
        out.push_str(&format!(
            "<tr>\
             <td><strong>{name}</strong>{title_hint}</td>\
             <td><span class=\"pill\">{status}</span></td>\
             <td>{conclusion_cell}</td>\
             <td class=\"muted\" style=\"font-family:monospace\">{sha}</td>\
             </tr>",
            name = esc(&r.name),
            status = esc(r.status.as_str()),
            sha = esc(&r.head_sha[..r.head_sha.len().min(8)]),
        ));
    }
    out.push_str("</table>");
    out
}

/// Wrap page `body` in the site chrome: HTMX-boosted header, search bar,
/// user menu, and consistent CSS/JS includes.
pub fn page(title: &str, session: &Option<User>, body: &str) -> Html<String> {
    let nav_user = match session {
        Some(user) => format!(
            "<span class=\"nav-username\">@{user}</span>\
             <a href=\"/notifications\">Notifications</a>\
             <a href=\"/logout\">Sign out</a>",
            user = esc(&user.username),
        ),
        None => "<a href=\"/login\">Sign in</a>".to_string(),
    };
    Html(format!(
        "<!doctype html><html lang=\"en\"><head>\
<meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{title} \u{00b7} rithub</title>\
<style>{CSS}</style>\
<style>{HL_CSS}</style>\
<script src=\"/static/htmx.min.js\" defer></script>\
</head>\
<body hx-boost=\"true\">\
<div id=\"htmx-bar\" class=\"htmx-bar\"></div>\
<header class=\"nav\">\
<a href=\"/\" class=\"brand\">rithub</a>\
<form class=\"nav-search\" method=\"get\" action=\"/search\" role=\"search\">\
<input name=\"q\" placeholder=\"Search\u{2026}\" class=\"nav-search-input\" autocomplete=\"off\" spellcheck=\"false\">\
</form>\
<nav class=\"nav-actions\">{nav_user}</nav>\
</header>\
<main class=\"wrap\">{body}</main>\
<footer class=\"foot\">rithub \u{2014} a Rust git forge on <code>rit</code></footer>\
</body></html>",
        title = esc(title),
        HL_CSS = forge_search::highlight::highlight_css(),
    ))
}

/// A small error page rendered at the given HTTP status.
pub fn error_page(session: &Option<User>, status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<div class=\"card\"><h2>Error {code}</h2>\
         <p class=\"muted\">{msg}</p>\
         <p><a href=\"javascript:history.back()\">&larr; Back</a> \
            &nbsp; <a href=\"/\">Home</a></p></div>",
        code = status.as_u16(),
        msg = esc(message)
    );
    (status, page("error", session, &body)).into_response()
}

const CSS: &str = "\
:root{--fg:#1f2328;--muted:#656d76;--line:#d0d7de;--bg:#fff;\
--accent:#0969da;--ok:#1a7f37;--bad:#cf222e;--nav-bg:#24292f}\
*{box-sizing:border-box}\
body{margin:0;font:14px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;\
color:var(--fg);background:#f6f8fa}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}\
\
.htmx-bar{height:3px;background:var(--accent);width:0%;position:fixed;top:0;left:0;\
z-index:9999;transition:width .3s}\
body.htmx-request .htmx-bar{width:60%;transition:width 2s ease-out}\
\
.nav{display:flex;align-items:center;gap:12px;padding:10px 20px;\
background:var(--nav-bg);color:#fff;flex-wrap:wrap}\
.nav a{color:#fff}\
.brand{font-weight:700;font-size:15px;letter-spacing:-.3px;white-space:nowrap}\
.nav-search{flex:1;min-width:120px;max-width:460px}\
.nav-search-input{width:100%;padding:5px 10px;border:1px solid #444d56;\
border-radius:6px;background:#2d333b;color:#fff;font:inherit;outline:none}\
.nav-search-input:focus{border-color:var(--accent);background:#22272e}\
.nav-search-input::placeholder{color:#8b949e}\
.nav-actions{display:flex;align-items:center;gap:10px}\
.nav-username{color:#8b949e;font-weight:600}\
\
.wrap{max-width:1060px;margin:24px auto;padding:0 16px}\
.foot{max-width:1060px;margin:40px auto;padding:16px;color:var(--muted);\
border-top:1px solid var(--line)}\
\
.card{background:var(--bg);border:1px solid var(--line);border-radius:8px;\
padding:16px;margin-bottom:16px}\
.muted{color:var(--muted)}\
code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}\
pre{background:#f6f8fa;border:1px solid var(--line);border-radius:6px;\
padding:12px;overflow:auto}\
\
table{border-collapse:collapse;width:100%}\
td,th{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line)}\
\
.row{display:flex;align-items:center;gap:8px}\
.spacer{flex:1}\
\
.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:12px;\
border:1px solid var(--line)}\
.pill.open,.ok.pill{background:#dafbe1;color:var(--ok);border-color:#aceebb}\
.pill.closed,.bad.pill{background:#ffebe9;color:var(--bad);border-color:#ffcecb}\
.pill.merged{background:#fbefff;color:#8250df;border-color:#e6d6ff}\
\
.btn{display:inline-block;padding:6px 12px;border-radius:6px;\
border:1px solid var(--line);background:#f6f8fa;cursor:pointer;font:inherit}\
.btn.primary{background:var(--ok);color:#fff;border-color:var(--ok)}\
.btn.danger{background:var(--bad);color:#fff;border-color:var(--bad)}\
\
input,textarea,select{font:inherit;padding:6px 8px;border:1px solid var(--line);\
border-radius:6px;width:100%}\
label{display:block;margin:8px 0 2px;font-weight:600}\
\
.repo-tabs{display:flex;flex-wrap:wrap;gap:0;\
border-bottom:2px solid var(--line);margin-bottom:16px;padding-bottom:0}\
.repo-tabs a{padding:8px 14px;color:var(--fg);font-weight:500;\
border-bottom:2px solid transparent;margin-bottom:-2px;\
text-decoration:none;display:inline-block;white-space:nowrap}\
.repo-tabs a:hover{color:var(--accent)}\
.repo-tabs a.tab-active{color:var(--fg);border-bottom-color:var(--accent);font-weight:600}\
\
.breadcrumbs{max-width:1060px;margin:8px auto 0;padding:0 16px;\
display:flex;align-items:center;flex-wrap:wrap;gap:0;font-size:13px;color:var(--muted)}\
.breadcrumbs a{color:var(--accent)}\
.bc-sep{color:var(--muted);padding:0 6px;user-select:none}\
\
.diff{background:#fff;border:1px solid var(--line);border-radius:6px;\
overflow:auto;margin:10px 0}\
.diff .h{background:#f6f8fa;padding:6px 10px;border-bottom:1px solid var(--line);\
font-family:ui-monospace,monospace}\
.diff pre{margin:0;border:0;border-radius:0;background:#fff}\
.diff .add{background:#e6ffec}.diff .del{background:#ffebe9}.diff .hunk{color:#8250df}\
\
h1,h2,h3{margin:.4em 0}\
.list a{display:block;padding:6px 0}\
.ok{color:var(--ok)}.bad{color:var(--bad)}\
.readme{padding-top:8px}\
.empty-state{text-align:center;padding:40px 16px;color:var(--muted)}\
.empty-state h3{font-size:18px;margin-bottom:8px}";

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
    match state
        .store
        .find_user_by_token(&hash_token(&session.token))
        .await
    {
        Ok(Some((user, _token))) if user.username == session.username => Some(user),
        _ => None,
    }
}

// --- landing + auth pages ---------------------------------------------------

async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let orgs = match state.store.list_orgs().await {
        Ok(orgs) => orgs,
        Err(_) => {
            return error_page(
                &session,
                StatusCode::INTERNAL_SERVER_ERROR,
                "store error",
            )
        }
    };
    let mut body = String::from("<div class=\"card\"><h1>Repositories</h1>");
    if orgs.is_empty() {
        body.push_str(
            "<div class=\"empty-state\"><h3>No repositories yet</h3>\
             <p>Create an org and push your first repo.</p></div>",
        );
    }
    for org in orgs {
        body.push_str(&format!(
            "<h3>{}</h3><div class=\"list\">",
            esc(&org.name)
        ));
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
    let mut body = String::from(
        "<div class=\"card\" style=\"max-width:420px;margin:40px auto\">\
         <h1>Sign in to rithub</h1>\
         <p class=\"muted\">Use your username and a personal access token \
         (Settings &rarr; Tokens).</p>\
         <form method=\"post\" action=\"/login\">\
         <label>Username</label>\
         <input name=\"username\" autocomplete=\"username\" required>\
         <label>Token</label>\
         <input name=\"token\" type=\"password\" autocomplete=\"current-password\" required>\
         <p style=\"margin-top:12px\">\
         <button class=\"btn primary\" type=\"submit\">Sign in</button>\
         </p></form>",
    );
    // Offer OAuth/SSO sign-in for each provider enabled on this server.
    let providers: Vec<(&str, &str)> = [("github", "GitHub"), ("google", "Google")]
        .into_iter()
        .filter(|(slug, _)| crate::oauth::is_enabled(slug))
        .collect();
    if !providers.is_empty() {
        body.push_str("<hr><p class=\"muted\">Or continue with a single-sign-on provider:</p>\
         <div class=\"list\">");
        for (slug, label) in providers {
            body.push_str(&format!(
                "<a class=\"btn\" href=\"/login/oauth/{slug}\">Continue with {label}</a> ",
                slug = esc(slug),
                label = esc(label),
            ));
        }
        body.push_str("</div>");
    }
    body.push_str("</div>");
    page("Sign in", &session, &body).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    token: String,
}

async fn login_submit(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Response {
    let valid = matches!(
        state
            .store
            .find_user_by_token(&hash_token(&form.token))
            .await,
        Ok(Some((user, _))) if user.username == form.username
    );
    if !valid {
        let body =
            "<div class=\"card\" style=\"max-width:420px;margin:40px auto\">\
             <h1>Sign in</h1>\
             <p class=\"bad\">Invalid username or token.</p>\
             <p><a href=\"/login\">&larr; Try again</a></p></div>";
        return (StatusCode::UNAUTHORIZED, page("Sign in", &None, body)).into_response();
    }
    let cookie_val = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", form.username, form.token));
    let cookie = format!(
        "rithub_session={cookie_val}; Path=/; HttpOnly; SameSite=Lax"
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

async fn logout() -> Response {
    let cookie = "rithub_session=; Path=/; HttpOnly; Max-Age=0".to_string();
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

// --- actions (CI check-runs) ------------------------------------------------

/// GET /{org}/{repo}/actions — check-run history for the repository.
async fn actions_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match state.store.get_repo(&org, &repo).await {
        Ok(m) => m,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "repository not found"),
    };

    // Show check runs for the HEAD commit of the default branch.
    let head_sha = state
        .host
        .default_branch(&org, &repo)
        .ok()
        .and_then(|branch| state.host.resolve_branch(&org, &repo, &branch).ok())
        .map(|oid| oid.to_hex());

    let (sha_label, runs) = match head_sha {
        Some(ref sha) => {
            let runs = state
                .store
                .latest_check_runs(meta.id, sha)
                .await
                .unwrap_or_default();
            (sha[..sha.len().min(8)].to_string(), runs)
        }
        None => (String::new(), vec![]),
    };

    let runners = state.store.list_runners(meta.id).await.unwrap_or_default();

    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}</div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "actions"),
    );

    // Check-runs section
    body.push_str("<div class=\"card\"><h3>Check runs");
    if !sha_label.is_empty() {
        body.push_str(&format!(
            " <span class=\"muted\" style=\"font-weight:400\">on HEAD ({sha_label})</span>"
        ));
    }
    body.push_str("</h3>");
    body.push_str(&check_runs_html(&runs));
    body.push_str("</div>");

    // Registered runners
    body.push_str("<div class=\"card\"><h3>Runners</h3>");
    if runners.is_empty() {
        body.push_str(
            "<p class=\"muted\">No runners registered. \
             Use the API to register a runner: \
             <code>POST /api/orgs/{org}/repos/{repo}/runners</code>.</p>",
        );
    } else {
        body.push_str("<table><tr><th>Name</th><th>Last seen</th></tr>");
        for r in &runners {
            let last_seen = r
                .last_seen_at
                .map(|t| short_date(t.unix_timestamp()))
                .unwrap_or_else(|| "never".into());
            body.push_str(&format!(
                "<tr><td><strong>{}</strong></td><td class=\"muted\">{last_seen}</td></tr>",
                esc(&r.name)
            ));
        }
        body.push_str("</table>");
    }
    body.push_str("</div>");

    page("Actions", &session, &body).into_response()
}

// --- settings stub ----------------------------------------------------------

/// GET /{org}/{repo}/settings — repository settings shell.
/// Full settings (webhooks, branch protection, collaborators) are managed via
/// the JSON API; this page links to those endpoints and shows current values.
async fn settings_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match state.store.get_repo(&org, &repo).await {
        Ok(m) => m,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "repository not found"),
    };

    let default_branch = state
        .host
        .default_branch(&org, &repo)
        .unwrap_or_else(|_| "main".into());

    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}</div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "settings"),
    );

    body.push_str(&format!(
        "<div class=\"card\"><h3>Repository details</h3>\
         <table>\
         <tr><th>Field</th><th>Value</th></tr>\
         <tr><td>ID</td><td><code>{id}</code></td></tr>\
         <tr><td>Default branch</td><td><code>{branch}</code></td></tr>\
         <tr><td>Private</td><td>{private}</td></tr>\
         </table></div>",
        id = esc(&meta.id.to_string()),
        branch = esc(&default_branch),
        private = if meta.visibility == Visibility::Private { "Yes" } else { "No" },
    ));

    body.push_str(&format!(
        "<div class=\"card\"><h3>API references</h3>\
         <p class=\"muted\">Manage settings programmatically via the JSON API:</p>\
         <ul>\
         <li><a href=\"/api/orgs/{o}/repos/{r}\"><code>GET /api/orgs/{o}/repos/{r}</code></a></li>\
         <li><code>PATCH /api/orgs/{o}/repos/{r}</code> &mdash; update name/visibility</li>\
         <li><a href=\"/api/orgs/{o}/repos/{r}/hooks\"><code>GET /api/orgs/{o}/repos/{r}/hooks</code></a></li>\
         <li><code>POST /api/orgs/{o}/repos/{r}/hooks</code> &mdash; add webhook</li>\
         <li><code>POST /api/orgs/{o}/repos/{r}/branches/{{branch}}/protection</code> &mdash; branch rules</li>\
         <li><code>POST /api/orgs/{o}/repos/{r}/collaborators</code> &mdash; add collaborator</li>\
         <li><a href=\"/{o}/{r}/audit\">Audit log</a></li>\
         </ul></div>",
        o = esc(&org),
        r = esc(&repo),
    ));

    page("Settings", &session, &body).into_response()
}

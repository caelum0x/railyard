//! Server-rendered web UI. Hand-rolled HTML (no template engine) with a strict
//! escaping discipline: **every** dynamic value goes through [`esc`], and README
//! markdown is rendered with raw-HTML stripped ([`markdown`]) so user content
//! cannot inject scripts.
//!
//! Pages render real data from the store + `rit` — never placeholder content.
//! Submodules:
//! - [`session`] — sign in / sign up / sign out, server-side web sessions;
//! - [`account`] — dashboard, new org / repo, org + user pages, user settings;
//! - [`repo`] — browse (tree, blob, raw, commits, commit diff), fork;
//! - [`collab`] — issues, milestones, labels;
//! - [`pulls`] — pull requests (diff, checks, reviews, conversation, merge);
//! - [`releases`] — releases, assets, source archives;
//! - [`settings`] — repository settings (general, access, branches, hooks, keys);
//! - [`actions`] — CI check runs and runners;
//! - [`governance`] — search, notifications, audit log.
//!
//! ## Authorization
//! Every repository page goes through [`web_repo_read`] (private repos are a
//! 404 to anyone without access) and every write through [`web_repo_role`] (or
//! a participant check), backed by the same [`crate::authz`] rules as the API.
//! Mutations share [`crate::collab_svc`] with the API so webhooks,
//! notifications, audit, and search indexing fire identically.
//!
//! ## HTMX integration
//! Vendored `htmx.min.js` is served from `/static/htmx.min.js`. `hx-boost` on
//! `<body>` turns links and forms into XHR swaps with graceful degradation;
//! handlers that see `HX-Request: true` ([`is_htmx`]) may return fragments.

mod account;
mod actions;
mod collab;
mod governance;
mod pulls;
mod releases;
mod repo;
mod session;
mod settings;
mod user_settings;

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use rithub_core::{RepoRole, Repository as RepoMeta, Store, User, Visibility};

use crate::error::AppError;
use crate::state::AppState;

pub use session::session_user;
pub(crate) use session::start_session_cookie;

/// Register every web route. Merged into the main router (which owns the git
/// and JSON-API routes) so `/static` and `/api` paths keep priority.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/static/htmx.min.js", get(htmx_js))
        // Top level + accounts
        .route("/", get(account::landing))
        .route("/login", get(session::login_form).post(session::login_submit))
        .route("/signup", get(session::signup_form).post(session::signup_submit))
        .route("/logout", get(session::logout_form).post(session::logout))
        .route("/new", get(account::new_repo_form).post(account::new_repo_submit))
        .route("/orgs/new", get(account::new_org_form).post(account::new_org_submit))
        .route("/settings", get(user_settings::settings_redirect))
        .route("/settings/profile", get(user_settings::profile_page))
        .route(
            "/settings/tokens",
            get(user_settings::tokens_page).post(user_settings::token_create),
        )
        .route("/settings/tokens/{id}/revoke", post(user_settings::token_revoke))
        .route("/settings/keys", get(user_settings::keys_page).post(user_settings::key_add))
        .route("/settings/keys/{id}/delete", post(user_settings::key_delete))
        // Search + notifications
        .route("/search", get(governance::search))
        .route("/search/results", get(governance::search_results))
        .route("/notifications", get(governance::notifications))
        .route("/notifications/read", post(governance::mark_read))
        .route("/notifications/{id}/read", post(governance::mark_one_read))
        // Org / user namespace page + org management
        .route("/{owner}", get(account::owner_page))
        .route("/{org}/-/members", post(account::member_add))
        .route("/{org}/-/members/{user}/remove", post(account::member_remove))
        .route("/{org}/-/teams", post(account::team_create))
        .route("/{org}/-/teams/{team}/members", post(account::team_member_add))
        .route("/{org}/-/teams/{team}/repos", post(account::team_repo_grant))
        .route(
            "/{org}/-/teams/{team}/members/{user}/remove",
            post(account::team_member_remove),
        )
        .route("/{org}/-/teams/{team}/repos/{repo}/revoke", post(account::team_repo_revoke))
        .route("/{org}/-/teams/{team}/delete", post(account::team_delete))
        // Repo: code browse
        .route("/{org}/{repo}", get(repo::home))
        .route("/{org}/{repo}/fork", post(repo::fork))
        .route("/{org}/{repo}/tree/{rev}", get(repo::tree_root))
        .route("/{org}/{repo}/tree/{rev}/{*path}", get(repo::tree_path))
        .route("/{org}/{repo}/blob/{rev}/{*path}", get(repo::blob))
        .route("/{org}/{repo}/raw/{rev}/{*path}", get(repo::raw))
        .route("/{org}/{repo}/archive/{file}", get(repo::archive))
        .route("/{org}/{repo}/commits/{rev}", get(repo::commits))
        .route("/{org}/{repo}/commit/{oid}", get(repo::commit_detail))
        // Repo: issues, labels, milestones
        .route("/{org}/{repo}/issues", get(collab::issues_list))
        .route("/{org}/{repo}/issues/new", get(collab::issue_new_form))
        .route("/{org}/{repo}/issues/create", post(collab::issue_create))
        .route("/{org}/{repo}/issues/{number}", get(collab::issue_detail))
        .route("/{org}/{repo}/issues/{number}/comment", post(collab::issue_comment))
        .route("/{org}/{repo}/issues/{number}/state", post(collab::issue_state))
        .route("/{org}/{repo}/issues/{number}/triage", post(collab::issue_triage))
        .route("/{org}/{repo}/labels", get(collab::labels_page).post(collab::label_create))
        .route("/{org}/{repo}/labels/{id}/edit", post(collab::label_edit))
        .route("/{org}/{repo}/labels/{id}/delete", post(collab::label_delete))
        .route(
            "/{org}/{repo}/milestones",
            get(collab::milestones_page).post(collab::milestone_create),
        )
        .route("/{org}/{repo}/milestones/{number}/state", post(collab::milestone_state))
        .route("/{org}/{repo}/milestones/{number}/delete", post(collab::milestone_delete))
        // Repo: pull requests
        .route("/{org}/{repo}/pulls", get(pulls::pulls_list))
        .route("/{org}/{repo}/pulls/new", get(pulls::pr_new_form))
        .route("/{org}/{repo}/pulls/create", post(pulls::pr_create))
        .route("/{org}/{repo}/pulls/{number}", get(pulls::pr_detail))
        .route("/{org}/{repo}/pulls/{number}/review", post(pulls::pr_review))
        .route("/{org}/{repo}/pulls/{number}/comment", post(pulls::pr_comment))
        .route("/{org}/{repo}/pulls/{number}/state", post(pulls::pr_state))
        .route("/{org}/{repo}/pulls/{number}/merge", post(pulls::pr_merge))
        .route("/{org}/{repo}/pulls/{number}/delete-branch", post(pulls::pr_delete_branch))
        // Repo: audit log
        .route("/{org}/{repo}/audit", get(governance::audit_log))
        // Repo: releases
        .route("/{org}/{repo}/releases", get(releases::list))
        .route("/{org}/{repo}/releases/new", get(releases::new_form))
        .route("/{org}/{repo}/releases/create", post(releases::create))
        .route(
            "/{org}/{repo}/releases/{tag}/edit",
            get(releases::edit_form).post(releases::edit_submit),
        )
        .route("/{org}/{repo}/releases/{tag}/delete", post(releases::delete))
        .route("/{org}/{repo}/releases/{tag}/archive.tar.gz", get(releases::archive))
        .route("/{org}/{repo}/releases/{tag}/assets/{id}", get(releases::asset_download))
        .route(
            "/{org}/{repo}/releases/{tag}/assets/{id}/delete",
            post(releases::asset_delete),
        )
        // Repo: actions (check runs + runners)
        .route("/{org}/{repo}/actions", get(actions::actions_page))
        .route("/{org}/{repo}/actions/runs/{id}", get(actions::run_detail))
        .route("/{org}/{repo}/actions/runs/{id}/rerun", post(actions::run_rerun))
        .route("/{org}/{repo}/actions/runs/{id}/cancel", post(actions::run_cancel))
        .route("/{org}/{repo}/actions/runners", post(actions::runner_register))
        .route("/{org}/{repo}/actions/runners/{id}/delete", post(actions::runner_delete))
        // Repo: settings
        .route("/{org}/{repo}/settings", get(settings::general_page))
        .route("/{org}/{repo}/settings/general", post(settings::general_submit))
        .route("/{org}/{repo}/settings/delete", post(settings::delete_submit))
        .route(
            "/{org}/{repo}/settings/access",
            get(settings::access_page).post(settings::collaborator_add),
        )
        .route(
            "/{org}/{repo}/settings/access/{user}/remove",
            post(settings::collaborator_remove),
        )
        .route(
            "/{org}/{repo}/settings/branches",
            get(settings::branches_page).post(settings::branch_protect),
        )
        .route("/{org}/{repo}/settings/branches/unprotect", post(settings::branch_unprotect))
        .route("/{org}/{repo}/settings/branches/delete", post(settings::branch_delete))
        .route(
            "/{org}/{repo}/settings/hooks",
            get(settings::hooks_page).post(settings::hook_create),
        )
        .route("/{org}/{repo}/settings/hooks/{id}", post(settings::hook_update))
        .route("/{org}/{repo}/settings/hooks/{id}/delete", post(settings::hook_delete))
        .route(
            "/{org}/{repo}/settings/keys",
            get(settings::keys_page).post(settings::deploy_key_create),
        )
        .route("/{org}/{repo}/settings/keys/{id}/delete", post(settings::deploy_key_delete))
}

// --- static assets -----------------------------------------------------------

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

/// Percent-encode a value for use inside a URL query string.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Render markdown to sanitized HTML — raw/inline HTML events are dropped so a
/// README cannot smuggle `<script>` or other markup.
pub fn markdown(md: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser, Tag};
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)))
        // Neutralize `javascript:` / `data:` link and image targets.
        .map(|event| match event {
            Event::Start(Tag::Link { link_type, dest_url, title, id }) if unsafe_url(&dest_url) => {
                Event::Start(Tag::Link { link_type, dest_url: "#".into(), title, id })
            }
            Event::Start(Tag::Image { link_type, dest_url, title, id }) if unsafe_url(&dest_url) => {
                Event::Start(Tag::Image { link_type, dest_url: "#".into(), title, id })
            }
            other => other,
        });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn unsafe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("vbscript:")
}

/// Format a unix timestamp as `YYYY-MM-DD`.
pub fn short_date(unix_seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix_seconds)
        .map(|dt| dt.date().to_string())
        .unwrap_or_default()
}

/// Returns `true` when the request originated from htmx (`HX-Request: true`).
pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").and_then(|v| v.to_str().ok()) == Some("true")
}

/// Inline SVG icons (no emoji glyphs). `name` selects the path set.
pub fn icon(name: &str) -> &'static str {
    match name {
        "folder" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M1.75 1A1.75 1.75 0 0 0 0 2.75v10.5C0 14.22.78 15 1.75 15h12.5A1.75 1.75 0 0 0 16 13.25v-8.5A1.75 1.75 0 0 0 14.25 3H7.5a.25.25 0 0 1-.2-.1l-.9-1.2c-.33-.44-.85-.7-1.4-.7Z\"/></svg>",
        "file" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M2 1.75C2 .78 2.78 0 3.75 0h6.59c.46 0 .9.18 1.23.51l2.92 2.92c.33.33.51.77.51 1.23v9.59A1.75 1.75 0 0 1 13.25 16h-9.5A1.75 1.75 0 0 1 2 14.25Zm1.75-.25a.25.25 0 0 0-.25.25v12.5c0 .14.11.25.25.25h9.5a.25.25 0 0 0 .25-.25V6h-2.75A1.75 1.75 0 0 1 9 4.25V1.5Zm6.75.06V4.25c0 .14.11.25.25.25h2.69Z\"/></svg>",
        "lock" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M4 4a4 4 0 0 1 8 0v2h.25c.97 0 1.75.78 1.75 1.75v5.5A1.75 1.75 0 0 1 12.25 15h-8.5A1.75 1.75 0 0 1 2 13.25v-5.5C2 6.78 2.78 6 3.75 6H4Zm6.5 2V4a2.5 2.5 0 0 0-5 0v2Z\"/></svg>",
        "repo" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M2 2.5A2.5 2.5 0 0 1 4.5 0h8.75a.75.75 0 0 1 .75.75v12.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1 0-1.5h1.75v-2h-8a1 1 0 0 0-.71 1.71.75.75 0 0 1-1.06 1.06A2.49 2.49 0 0 1 2 11.5Zm10.5-1h-8a1 1 0 0 0-1 1v6.71A2.5 2.5 0 0 1 4.5 9h8Z\"/></svg>",
        "dot" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><circle cx=\"8\" cy=\"8\" r=\"4\"/></svg>",
        "fork" => "<svg class=\"icon\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"M5 5.37v.88c0 .41.34.75.75.75h4.5a.75.75 0 0 0 .75-.75v-.88a2.25 2.25 0 1 1 1.5 0v.88A2.25 2.25 0 0 1 10.25 8.5h-1.5v2.13a2.25 2.25 0 1 1-1.5 0V8.5h-1.5A2.25 2.25 0 0 1 3.5 6.25v-.88a2.25 2.25 0 1 1 1.5 0ZM5 3.25a.75.75 0 1 0-1.5 0 .75.75 0 0 0 1.5 0Zm6.75.75a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Zm-3 8.75a.75.75 0 1 0-1.5 0 .75.75 0 0 0 1.5 0Z\"/></svg>",
        _ => "",
    }
}

/// The repo tab bar. The Settings tab is shown only to repo admins.
pub fn tabs_for(org: &str, repo: &str, active: &str, role: Option<RepoRole>) -> String {
    let tab = |name: &str, href: String, key: &str| -> String {
        let cls = if key == active { " class=\"tab-active\"" } else { "" };
        format!("<a href=\"{href}\"{cls}>{name}</a>")
    };
    let (o, r) = (esc(org), esc(repo));
    let mut out = String::from("<nav class=\"repo-tabs\">");
    out.push_str(&tab("Code", format!("/{o}/{r}"), "code"));
    out.push_str(&tab("Issues", format!("/{o}/{r}/issues"), "issues"));
    out.push_str(&tab("Pull requests", format!("/{o}/{r}/pulls"), "pulls"));
    out.push_str(&tab("Actions", format!("/{o}/{r}/actions"), "actions"));
    out.push_str(&tab("Releases", format!("/{o}/{r}/releases"), "releases"));
    out.push_str(&tab("Milestones", format!("/{o}/{r}/milestones"), "milestones"));
    if role.is_some_and(|r| r.can_admin()) {
        out.push_str(&tab("Settings", format!("/{o}/{r}/settings"), "settings"));
    }
    out.push_str("</nav>");
    out
}

/// The standard repo header card: `org / repo`, visibility, and the tab bar.
pub fn repo_header(org: &str, repo: &str, meta: &RepoMeta, active: &str, role: Option<RepoRole>) -> String {
    let vis = if meta.visibility == Visibility::Private {
        format!("<span class=\"pill\">{} Private</span>", icon("lock"))
    } else {
        "<span class=\"pill\">Public</span>".to_string()
    };
    let desc = meta
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| format!("<p class=\"muted\">{}</p>", esc(d)))
        .unwrap_or_default();
    format!(
        "<div class=\"card\"><h1>{icon} <a href=\"/{o}\">{o}</a> / <a href=\"/{o}/{r}\">{r}</a> {vis}</h1>{desc}{tabs}</div>",
        icon = icon("repo"),
        o = esc(org),
        r = esc(repo),
        tabs = tabs_for(org, repo, active, role),
    )
}

/// Breadcrumb navigation strip. `None` href marks the terminal segment.
pub fn breadcrumbs(parts: &[(&str, Option<&str>)]) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let mut out = String::from("<nav class=\"breadcrumbs\" aria-label=\"breadcrumb\">");
    for (i, (label, href)) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str("<span class=\"bc-sep\">/</span>");
        }
        match href {
            Some(h) => out.push_str(&format!("<a href=\"{}\">{}</a>", esc(h), esc(label))),
            None => out.push_str(&format!("<span aria-current=\"page\">{}</span>", esc(label))),
        }
    }
    out.push_str("</nav>");
    out
}

/// Render a slice of check runs as a compact status table.
pub fn check_runs_html(org: &str, repo: &str, runs: &[rithub_core::CheckRun]) -> String {
    if runs.is_empty() {
        return "<p class=\"muted\">No check runs yet.</p>".into();
    }
    let mut out = String::from(
        "<table><tr><th>Check</th><th>Status</th><th>Conclusion</th><th>Commit</th></tr>",
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
            rithub_core::CheckRunStatus::InProgress => ("muted", "in progress"),
            rithub_core::CheckRunStatus::Queued => ("muted", "queued"),
        };
        let title_hint = r
            .output_title
            .as_deref()
            .map(|t| format!("<br><span class=\"muted\">{}</span>", esc(t)))
            .unwrap_or_default();
        out.push_str(&format!(
            "<tr><td><a href=\"/{o}/{rp}/actions/runs/{id}\"><strong>{name}</strong></a>{title_hint}</td>\
             <td><span class=\"pill\">{status}</span></td>\
             <td><span class=\"{status_cls}\">{conclusion}</span></td>\
             <td class=\"mono muted\">{sha}</td></tr>",
            o = esc(org),
            rp = esc(repo),
            id = r.id,
            name = esc(&r.name),
            status = esc(r.status.as_str()),
            conclusion = esc(conclusion_label),
            sha = esc(&r.head_sha[..r.head_sha.len().min(8)]),
        ));
    }
    out.push_str("</table>");
    out
}

/// A one-line status message shown at the top of a page after a redirect.
pub fn notice_html(code: Option<&str>) -> String {
    let msg = match code {
        Some("saved") => "Changes saved.",
        Some("created") => "Created.",
        Some("deleted") => "Deleted.",
        Some("revoked") => "Token revoked.",
        Some("added") => "Added.",
        Some("removed") => "Removed.",
        Some("unprotected") => "Branch protection removed.",
        Some("branch-deleted") => "Branch deleted.",
        Some("rerun") => "A new run was queued.",
        Some("cancelled") => "Run cancelled.",
        Some("welcome") => "Welcome to rithub. Your account and personal organization are ready.",
        _ => return String::new(),
    };
    format!("<div class=\"notice ok-notice\" role=\"status\">{msg}</div>")
}

/// Wrap page `body` in the site chrome.
pub fn page(title: &str, session: &Option<User>, body: &str) -> Html<String> {
    let nav_user = match session {
        Some(user) => format!(
            "<a href=\"/new\" class=\"nav-new\">New repository</a>\
             <a href=\"/notifications\">Notifications</a>\
             <a href=\"/{u}\" class=\"nav-username\">@{u}</a>\
             <a href=\"/settings/profile\">Settings</a>\
             <form method=\"post\" action=\"/logout\" class=\"inline-form\">\
             <button type=\"submit\" class=\"linkish\">Sign out</button></form>",
            u = esc(&user.username),
        ),
        None => {
            let signup = if crate::authz::open_signup() {
                "<a href=\"/signup\" class=\"nav-new\">Sign up</a>"
            } else {
                ""
            };
            format!("<a href=\"/login\">Sign in</a>{signup}")
        }
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
<input name=\"q\" placeholder=\"Search\u{2026}\" class=\"nav-search-input\" autocomplete=\"off\" spellcheck=\"false\" aria-label=\"Search\">\
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
    let sign_in = if session.is_none() && status == StatusCode::UNAUTHORIZED {
        "<p><a class=\"btn primary\" href=\"/login\">Sign in</a></p>"
    } else {
        ""
    };
    let body = format!(
        "<div class=\"card\"><h2>{code} \u{00b7} {reason}</h2>\
         <p class=\"muted\">{msg}</p>{sign_in}\
         <p><a href=\"/\">Home</a></p></div>",
        code = status.as_u16(),
        reason = esc(status.canonical_reason().unwrap_or("Error")),
        msg = esc(message)
    );
    (status, page("Error", session, &body)).into_response()
}

/// Render an [`AppError`] as an error page.
pub fn app_error_page(session: &Option<User>, error: AppError) -> Response {
    error_page(session, error.status(), &error.client_message())
}

/// Redirect to sign-in, returning to `next` afterwards.
pub fn login_redirect(next: &str) -> Response {
    Redirect::to(&format!("/login?next={}", url_encode(next))).into_response()
}

/// Sanitize a post-login redirect target (same-site absolute paths only).
pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_string(),
        _ => "/".to_string(),
    }
}

/// External base URL (`RITHUB_PUBLIC_URL`, else derived from proxy headers).
pub fn public_base(headers: &HeaderMap) -> String {
    if let Ok(url) = std::env::var("RITHUB_PUBLIC_URL") {
        if !url.trim().is_empty() {
            return url.trim().trim_end_matches('/').to_string();
        }
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .filter(|p| *p == "https" || *p == "http")
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

// --- repository access for web handlers --------------------------------------

/// Load a repo the viewer may read, plus their role. Unreadable = 404 page.
pub async fn web_repo_read(
    state: &AppState,
    session: &Option<User>,
    org: &str,
    repo: &str,
) -> Result<(RepoMeta, Option<RepoRole>), Response> {
    let not_found = || error_page(session, StatusCode::NOT_FOUND, "repository not found");
    let meta = state.store.get_repo(org, repo).await.map_err(|_| not_found())?;
    let role = match session {
        Some(user) => crate::authz::user_repo_role(state, user, &meta).await,
        None => None,
    };
    if meta.visibility == Visibility::Private && role.is_none() {
        return Err(not_found());
    }
    Ok((meta, role))
}

/// Load a repo on which the signed-in viewer holds ≥`needed`. Anonymous viewers
/// are sent to sign in; insufficient roles get a 403 page.
pub async fn web_repo_role(
    state: &AppState,
    session: &Option<User>,
    org: &str,
    repo: &str,
    needed: RepoRole,
    next: &str,
) -> Result<(RepoMeta, User), Response> {
    let (meta, role) = web_repo_read(state, session, org, repo).await?;
    let Some(user) = session.clone() else {
        return Err(login_redirect(next));
    };
    match role {
        Some(r) if r >= needed => Ok((meta, user)),
        _ => Err(error_page(
            session,
            StatusCode::FORBIDDEN,
            &format!("this action requires {} access to {org}/{repo}", needed.as_str()),
        )),
    }
}

/// Load a readable repo for a signed-in participant (comment, open, review).
pub async fn web_repo_participant(
    state: &AppState,
    session: &Option<User>,
    org: &str,
    repo: &str,
    next: &str,
) -> Result<(RepoMeta, User, Option<RepoRole>), Response> {
    let (meta, role) = web_repo_read(state, session, org, repo).await?;
    let Some(user) = session.clone() else {
        return Err(login_redirect(next));
    };
    Ok((meta, user, role))
}

const CSS: &str = "\
:root{--fg:#1f2328;--muted:#59636e;--line:#d0d7de;--bg:#fff;\
--accent:#0969da;--ok:#1a7f37;--bad:#cf222e;--nav-bg:#24292f}\
*{box-sizing:border-box}\
body{margin:0;font:14px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;\
color:var(--fg);background:#f6f8fa}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}\
.icon{width:16px;height:16px;vertical-align:-3px;fill:currentColor;display:inline-block}\
.htmx-bar{height:3px;background:var(--accent);width:0%;position:fixed;top:0;left:0;\
z-index:9999;transition:width .3s}\
body.htmx-request .htmx-bar{width:60%;transition:width 2s ease-out}\
.nav{display:flex;align-items:center;gap:12px;padding:10px 20px;\
background:var(--nav-bg);color:#fff;flex-wrap:wrap}\
.nav a,.nav .linkish{color:#fff}\
.brand{font-weight:700;font-size:15px;letter-spacing:-.3px;white-space:nowrap}\
.nav-search{flex:1;min-width:120px;max-width:460px}\
.nav-search-input{width:100%;padding:5px 10px;border:1px solid #444d56;\
border-radius:6px;background:#2d333b;color:#fff;font:inherit;outline:none}\
.nav-search-input:focus{border-color:var(--accent);background:#22272e}\
.nav-search-input::placeholder{color:#8b949e}\
.nav-actions{display:flex;align-items:center;gap:12px;flex-wrap:wrap}\
.nav-username{color:#c9d1d9!important;font-weight:600}\
.nav-new{border:1px solid #57606a;border-radius:6px;padding:2px 8px}\
.inline-form{display:inline;margin:0}\
.linkish{background:none;border:0;padding:0;font:inherit;cursor:pointer;color:var(--accent)}\
.linkish:hover{text-decoration:underline}\
.wrap{max-width:1060px;margin:24px auto;padding:0 16px}\
.foot{max-width:1060px;margin:40px auto;padding:16px;color:var(--muted);\
border-top:1px solid var(--line)}\
.card{background:var(--bg);border:1px solid var(--line);border-radius:8px;\
padding:16px;margin-bottom:16px}\
.muted{color:var(--muted)}\
.mono,code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}\
pre{background:#f6f8fa;border:1px solid var(--line);border-radius:6px;\
padding:12px;overflow:auto}\
table{border-collapse:collapse;width:100%}\
td,th{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
.row{display:flex;align-items:center;gap:8px;flex-wrap:wrap}\
.spacer{flex:1}\
.grid2{display:grid;grid-template-columns:1fr 1fr;gap:16px}\
@media(max-width:720px){.grid2{grid-template-columns:1fr}}\
.pill{display:inline-block;padding:1px 8px;border-radius:20px;font-size:12px;\
border:1px solid var(--line);font-weight:500;vertical-align:middle}\
.pill.open,.ok.pill{background:#dafbe1;color:var(--ok);border-color:#aceebb}\
.pill.closed,.bad.pill{background:#ffebe9;color:var(--bad);border-color:#ffcecb}\
.pill.merged{background:#fbefff;color:#8250df;border-color:#e6d6ff}\
.btn{display:inline-block;padding:5px 12px;border-radius:6px;\
border:1px solid var(--line);background:#f6f8fa;cursor:pointer;font:inherit;color:var(--fg)}\
.btn:hover{text-decoration:none;background:#eef1f4}\
.btn.primary{background:var(--ok);color:#fff;border-color:var(--ok)}\
.btn.danger{background:var(--bad);color:#fff;border-color:var(--bad)}\
.btn.small{padding:2px 8px;font-size:12px}\
input,textarea,select{font:inherit;padding:6px 8px;border:1px solid var(--line);\
border-radius:6px;width:100%;background:#fff}\
input[type=checkbox],input[type=radio]{width:auto}\
label{display:block;margin:10px 0 2px;font-weight:600}\
label.check{font-weight:400;display:flex;align-items:center;gap:6px}\
.hint{font-size:12px;color:var(--muted);margin:2px 0 0}\
.notice{padding:10px 14px;border-radius:6px;margin-bottom:16px;border:1px solid}\
.ok-notice{background:#dafbe1;border-color:#aceebb;color:#116329}\
.bad-notice{background:#ffebe9;border-color:#ffcecb;color:#82071e}\
.secret{background:#fff8c5;border:1px solid #d4a72c;border-radius:6px;padding:10px;word-break:break-all}\
.repo-tabs{display:flex;flex-wrap:wrap;border-bottom:2px solid var(--line);margin-top:8px}\
.repo-tabs a{padding:8px 14px;color:var(--fg);font-weight:500;\
border-bottom:2px solid transparent;margin-bottom:-2px;display:inline-block;white-space:nowrap}\
.repo-tabs a:hover{color:var(--accent);text-decoration:none}\
.repo-tabs a.tab-active{border-bottom-color:#fd8c73;font-weight:600}\
.subnav{display:flex;gap:6px;flex-wrap:wrap;margin-bottom:16px}\
.subnav a{padding:4px 10px;border-radius:6px;color:var(--fg)}\
.subnav a.active{background:var(--accent);color:#fff}\
.breadcrumbs{display:flex;align-items:center;flex-wrap:wrap;font-size:13px;color:var(--muted);margin-bottom:8px}\
.bc-sep{color:var(--muted);padding:0 6px;user-select:none}\
.diff{background:#fff;border:1px solid var(--line);border-radius:6px;overflow:auto;margin:10px 0}\
.diff .h{background:#f6f8fa;padding:6px 10px;border-bottom:1px solid var(--line);\
font-family:ui-monospace,monospace}\
.diff pre{margin:0;border:0;border-radius:0;background:#fff}\
h1,h2,h3{margin:.4em 0}h1{font-size:20px}\
.list a{display:block;padding:6px 0}\
.ok{color:var(--ok)}.bad{color:var(--bad)}\
.readme{padding-top:8px;overflow-wrap:anywhere}\
.comment{border:1px solid var(--line);border-radius:6px;margin-bottom:10px}\
.comment-h{background:#f6f8fa;padding:6px 12px;border-bottom:1px solid var(--line);font-size:13px}\
.comment-b{padding:8px 12px}\
.danger-zone{border-color:var(--bad)}\
.empty-state{text-align:center;padding:40px 16px;color:var(--muted)}\
.empty-state h3{font-size:18px;margin-bottom:8px}\
.label-chip{display:inline-block;padding:0 8px;border-radius:20px;font-size:12px;font-weight:600;border:1px solid rgba(0,0,0,.15)}\
.plain{list-style:none;padding-left:0;margin:4px 0 10px}.plain li{padding:3px 0}";

/// A colored label chip. `color` is a stored 6-hex value.
pub fn label_chip(name: &str, color: &str) -> String {
    let c: String = color.chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
    let c = if c.len() == 6 { c } else { "cccccc".to_string() };
    let r = u8::from_str_radix(&c[0..2], 16).unwrap_or(204) as u32;
    let g = u8::from_str_radix(&c[2..4], 16).unwrap_or(204) as u32;
    let b = u8::from_str_radix(&c[4..6], 16).unwrap_or(204) as u32;
    let fg = if (r * 299 + g * 587 + b * 114) / 1000 > 150 { "#1f2328" } else { "#fff" };
    format!(
        "<span class=\"label-chip\" style=\"background:#{c};color:{fg}\">{}</span>",
        esc(name)
    )
}

/// Resolve a user id to a display username (cached per call site by caller).
pub async fn username_of(state: &AppState, id: Option<uuid::Uuid>) -> String {
    use rithub_core::OauthStore;
    match id {
        Some(id) => state
            .store
            .get_user_by_id(id)
            .await
            .ok()
            .flatten()
            .map(|u| u.username)
            .unwrap_or_else(|| "deleted user".to_string()),
        None => "unknown".to_string(),
    }
}

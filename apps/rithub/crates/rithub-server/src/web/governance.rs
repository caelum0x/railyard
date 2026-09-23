//! Web pages for global search, in-app notifications, and the audit log.
//!
//! ## HTMX live search
//! `GET /search` renders the full page; the input carries
//! `hx-get="/search/results"` so typing swaps just the `#search-results`
//! fragment. Without htmx the form submits normally (graceful degradation).
//! Results are permission-scoped: private repositories appear only for users
//! with access.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use rithub_core::{AuditStore, DirectoryStore, NotifyStore, RepoRole};
use serde::Deserialize;
use uuid::Uuid;

use super::{esc, login_redirect, page, repo_header, session_user, short_date, web_repo_role};
use crate::state::AppState;

/// GET /notifications — the signed-in user's in-app notifications.
pub async fn notifications(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/notifications");
    };
    let items = state.store.list_for_user(user.id, 100).await.unwrap_or_default();
    let unread = state.store.unread_count(user.id).await.unwrap_or(0);
    let mut body = format!(
        "<div class=\"card\"><div class=\"row\"><h1>Notifications</h1>\
         <span class=\"muted\">{unread} unread</span><span class=\"spacer\"></span>\
         <form method=\"post\" action=\"/notifications/read\" class=\"inline-form\">\
         <button class=\"btn small\" type=\"submit\">Mark all as read</button></form></div></div>\
         <div class=\"card\">"
    );
    if items.is_empty() {
        body.push_str(
            "<div class=\"empty-state\"><h3>No notifications</h3>\
             <p>Activity on repositories you belong to will appear here.</p></div>",
        );
    }
    for n in items {
        let link = n
            .link
            .as_deref()
            .filter(|l| l.starts_with('/') && !l.starts_with("//"))
            .unwrap_or("/notifications");
        let (marker, action) = if n.read {
            (String::new(), String::new())
        } else {
            (
                format!("<span class=\"bad\" title=\"unread\">{}</span> ", super::icon("dot")),
                format!(
                    "<form method=\"post\" action=\"/notifications/{id}/read\" class=\"inline-form\">\
                     <button class=\"btn small\" type=\"submit\">Mark read</button></form>",
                    id = n.id
                ),
            )
        };
        body.push_str(&format!(
            "<div class=\"row\" style=\"padding:8px 0;border-bottom:1px solid var(--line)\">\
             {marker}<span class=\"pill\">{kind}</span> <a href=\"{link}\">{title}</a>\
             <span class=\"muted\">{date}</span><span class=\"spacer\"></span>{action}</div>",
            kind = esc(&n.kind),
            link = esc(link),
            title = esc(&n.title),
            date = short_date(n.created_at.unix_timestamp())
        ));
    }
    body.push_str("</div>");
    page("Notifications", &session, &body).into_response()
}

/// POST /notifications/read — mark all read.
pub async fn mark_read(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/notifications");
    };
    if let Err(e) = state.store.mark_all_read(user.id).await {
        return super::app_error_page(&session, e.into());
    }
    Redirect::to("/notifications").into_response()
}

/// POST /notifications/{id}/read — mark one read.
pub async fn mark_one_read(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/notifications");
    };
    if let Err(e) = state.store.mark_notification_read(user.id, id).await {
        return super::app_error_page(&session, e.into());
    }
    Redirect::to("/notifications").into_response()
}

/// GET /{org}/{repo}/audit — the append-only audit log (repo admins only).
pub async fn audit_log(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/audit");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Admin, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let events = state.store.list_repo_events(meta.id, 200).await.unwrap_or_default();
    let mut body = repo_header(&org, &repo, &meta, "settings", Some(RepoRole::Admin));
    body.push_str(
        "<div class=\"card\"><h3>Audit log</h3>\
         <p class=\"muted\">Append-only record of security-relevant events.</p><table>\
         <tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Detail</th></tr>",
    );
    if events.is_empty() {
        body.push_str("<tr><td colspan=\"5\" class=\"muted\" style=\"padding:20px 8px\">No events yet.</td></tr>");
    }
    for e in events {
        body.push_str(&format!(
            "<tr><td class=\"muted\">{when}</td><td>{actor}</td><td><code>{action}</code></td>\
             <td>{target}</td><td class=\"muted\">{detail}</td></tr>",
            when = short_date(e.created_at.unix_timestamp()),
            actor = esc(&e.actor_name),
            action = esc(&e.action),
            target = esc(e.target.as_deref().unwrap_or("")),
            detail = esc(e.detail.as_deref().unwrap_or(""))
        ));
    }
    body.push_str("</table></div>");
    page("Audit log", &session, &body).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
}

/// Run the search query and return the results HTML fragment (no page chrome).
async fn run_search(state: &AppState, session: &Option<rithub_core::User>, query: &str) -> String {
    if query.is_empty() {
        return "<div id=\"search-results\"></div>".to_string();
    }
    let mut scope = forge_search::SearchScope::anonymous();
    if let Some(user) = session {
        scope.visible_private_repo_ids =
            crate::search_index::accessible_private_repo_ids(state, user.id).await;
    }
    let hits = crate::search_index::search(state, query, &scope, 50)
        .await
        .unwrap_or_default();

    let mut out = format!(
        "<div id=\"search-results\"><h3>{count} result(s) for \u{201c}{q}\u{201d}</h3>",
        count = hits.len(),
        q = esc(query),
    );
    if hits.is_empty() {
        out.push_str("<p class=\"muted\">No matches found.</p>");
    }
    // Resolve each repo's default branch once for blob links.
    let mut default_branches: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for hit in &hits {
        let (org_e, repo_e) = (esc(&hit.org), esc(&hit.repo));
        let href = match hit.kind.as_str() {
            "issue" => format!("/{org_e}/{repo_e}/issues/{}", hit.number),
            "pull_request" => format!("/{org_e}/{repo_e}/pulls/{}", hit.number),
            "commit" => format!("/{org_e}/{repo_e}/commit/{}", esc(&hit.oid)),
            _ if !hit.path.is_empty() => {
                let key = (hit.org.clone(), hit.repo.clone());
                let rev = default_branches
                    .entry(key)
                    .or_insert_with(|| {
                        state
                            .host
                            .default_branch(&hit.org, &hit.repo)
                            .unwrap_or_else(|_| "main".into())
                    })
                    .clone();
                format!("/{org_e}/{repo_e}/blob/{}/{}", esc(&rev), esc(&hit.path))
            }
            _ => format!("/{org_e}/{repo_e}"),
        };
        let label = if hit.number > 0 {
            format!("{org_e}/{repo_e} #{} &mdash; {}", hit.number, esc(&hit.title))
        } else if !hit.path.is_empty() {
            format!("{org_e}/{repo_e} &mdash; <span class=\"mono\">{}</span>", esc(&hit.path))
        } else {
            format!("{org_e}/{repo_e} &mdash; {}", esc(&hit.title))
        };
        let snippet = if hit.snippet.is_empty() {
            String::new()
        } else {
            format!("<div class=\"muted\" style=\"font-size:12px;margin-top:2px\">{}</div>", esc(&hit.snippet))
        };
        out.push_str(&format!(
            "<div style=\"margin:10px 0;padding:8px 0;border-bottom:1px solid var(--line)\">\
             <span class=\"pill\">{kind}</span> <a href=\"{href}\">{label}</a>{snippet}</div>",
            kind = esc(&hit.kind),
        ));
    }
    out.push_str("</div>");
    out
}

/// GET /search?q=... — global search page.
pub async fn search(
    State(state): State<AppState>,
    Query(form): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let query = form.q.trim().to_string();
    let results_block = run_search(&state, &session, &query).await;
    let body = format!(
        "<div class=\"card\"><h1>Search</h1>\
         <form method=\"get\" action=\"/search\"><div class=\"row\">\
         <input name=\"q\" value=\"{q}\" aria-label=\"Search query\" \
                placeholder=\"Search repositories, issues, pull requests, commits, code\u{2026}\" \
                hx-get=\"/search/results\" hx-trigger=\"keyup changed delay:350ms, search\" \
                hx-target=\"#search-results\" hx-swap=\"outerHTML\" style=\"flex:1\">\
         <button class=\"btn primary\" type=\"submit\">Search</button></div></form></div>\
         <div class=\"card\">{results_block}</div>",
        q = esc(&query),
    );
    page("Search", &session, &body).into_response()
}

/// GET /search/results?q=... — HTMX partial: the `#search-results` fragment.
pub async fn search_results(
    State(state): State<AppState>,
    Query(form): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let query = form.q.trim().to_string();
    Html(run_search(&state, &session, &query).await).into_response()
}

//! Web pages for the audit log, search, and notifications.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{AuditStore, NotifyStore, SearchStore, Store};
use serde::Deserialize;

use super::{error_page, esc, page, session_user, short_date};
use crate::state::AppState;

/// GET /notifications — the signed-in user's in-app notifications.
pub async fn notifications(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return error_page(&session, StatusCode::UNAUTHORIZED, "sign in to view notifications");
    };
    let items = state.store.list_for_user(user.id, 100).await.unwrap_or_default();
    let unread = state.store.unread_count(user.id).await.unwrap_or(0);
    let mut body = format!(
        "<div class=\"card\"><h1>Notifications</h1><p class=\"muted\">{unread} unread · \
<a href=\"/notifications/read\">mark all read</a></p></div><div class=\"card\">"
    );
    if items.is_empty() {
        body.push_str("<p class=\"muted\">No notifications.</p>");
    }
    for n in items {
        let dot = if n.read { "" } else { "● " };
        let link = n.link.unwrap_or_else(|| "#".into());
        body.push_str(&format!(
            "<div>{dot}<span class=\"pill\">{kind}</span> <a href=\"{link}\">{title}</a> \
<span class=\"muted\">{date}</span></div>",
            kind = esc(&n.kind),
            link = esc(&link),
            title = esc(&n.title),
            date = short_date(n.created_at.unix_timestamp())
        ));
    }
    body.push_str("</div>");
    page("Notifications", &session, &body).into_response()
}

/// GET /notifications/read — mark all read, then back to the list.
pub async fn mark_read(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    if let Some(user) = &session {
        let _ = state.store.mark_all_read(user.id).await;
    }
    Redirect::to("/notifications").into_response()
}

/// GET /{org}/{repo}/audit — the append-only audit log for a repo.
pub async fn audit_log(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match state.store.get_repo(&org, &repo).await {
        Ok(m) => m,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "repository not found"),
    };
    let events = state.store.list_repo_events(meta.id, 200).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1><h3>Audit log</h3>\
<p class=\"muted\">Append-only record of security-relevant events.</p></div>",
        esc(&org),
        esc(&repo)
    );
    body.push_str("<div class=\"card\"><table><tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Detail</th></tr>");
    if events.is_empty() {
        body.push_str("<tr><td colspan=\"5\" class=\"muted\">No events.</td></tr>");
    }
    for e in events {
        body.push_str(&format!(
            "<tr><td class=\"muted\">{when}</td><td>{actor}</td><td><code>{action}</code></td><td>{target}</td><td class=\"muted\">{detail}</td></tr>",
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
pub struct SearchForm {
    #[serde(default)]
    pub q: String,
}

/// GET /search?q=... — global search across repos, issues, and PRs.
pub async fn search(
    State(state): State<AppState>,
    Query(form): Query<SearchForm>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let query = form.q.trim().to_string();
    let mut body = format!(
        "<div class=\"card\"><h1>Search</h1><form method=\"get\" action=\"/search\">\
<input name=\"q\" value=\"{}\" placeholder=\"Search repos, issues, PRs\">\
<p><button class=\"btn primary\" type=\"submit\">Search</button></p></form></div>",
        esc(&query)
    );
    if !query.is_empty() {
        let hits = state.store.search(&query, 50).await.unwrap_or_default();
        body.push_str(&format!(
            "<div class=\"card\"><h3>{} result(s) for “{}”</h3>",
            hits.len(),
            esc(&query)
        ));
        if hits.is_empty() {
            body.push_str("<p class=\"muted\">No matches.</p>");
        }
        for hit in hits {
            let href = match hit.kind.as_str() {
                "issue" => format!("/{}/{}/issues/{}", esc(&hit.org), esc(&hit.repo), hit.number),
                "pull_request" => format!("/{}/{}/pulls/{}", esc(&hit.org), esc(&hit.repo), hit.number),
                _ => format!("/{}/{}", esc(&hit.org), esc(&hit.repo)),
            };
            let label = if hit.number > 0 {
                format!("{}/{} #{} — {}", esc(&hit.org), esc(&hit.repo), hit.number, esc(&hit.title))
            } else {
                format!("{}/{} — {}", esc(&hit.org), esc(&hit.repo), esc(&hit.title))
            };
            body.push_str(&format!(
                "<div><span class=\"pill\">{}</span> <a href=\"{href}\">{label}</a></div>",
                esc(&hit.kind)
            ));
        }
        body.push_str("</div>");
    }
    page("Search", &session, &body).into_response()
}

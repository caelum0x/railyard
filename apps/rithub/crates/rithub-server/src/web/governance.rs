//! Web pages for the audit log, global search, and in-app notifications.
//!
//! ## HTMX live search
//! `GET /search` renders the full search page (input + results). The search
//! input carries `hx-get="/search/results"` so as the user types, HTMX sends a
//! request to the `search_results` handler which returns just the `#search-results`
//! `<div>` fragment — no page chrome. Without htmx the form submits normally
//! and the full `/search` page renders with the results inline (graceful
//! degradation).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use rithub_core::{AuditStore, NotifyStore, Store};
use serde::Deserialize;

use super::{error_page, esc, page, session_user, short_date};
use crate::state::AppState;

/// GET /notifications — the signed-in user's in-app notifications.
pub async fn notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return error_page(
            &session,
            StatusCode::UNAUTHORIZED,
            "sign in to view notifications",
        );
    };
    let items = state
        .store
        .list_for_user(user.id, 100)
        .await
        .unwrap_or_default();
    let unread = state.store.unread_count(user.id).await.unwrap_or(0);
    let mut body = format!(
        "<div class=\"card\">\
         <h1>Notifications</h1>\
         <p class=\"muted\">{unread} unread &nbsp;\
         <a href=\"/notifications/read\">Mark all read</a></p>\
         </div>\
         <div class=\"card\">",
    );
    if items.is_empty() {
        body.push_str(
            "<div class=\"empty-state\"><h3>No notifications</h3>\
             <p>Activity on repos you watch will appear here.</p></div>",
        );
    }
    for n in items {
        let dot = if n.read { "" } else { "&#x25CF; " };
        let link = n.link.as_deref().unwrap_or("#");
        body.push_str(&format!(
            "<div style=\"padding:6px 0;border-bottom:1px solid var(--line)\">\
             {dot}<span class=\"pill\">{kind}</span> \
             <a href=\"{link}\">{title}</a> \
             <span class=\"muted\">{date}</span>\
             </div>",
            kind = esc(&n.kind),
            link = esc(link),
            title = esc(&n.title),
            date = short_date(n.created_at.unix_timestamp())
        ));
    }
    body.push_str("</div>");
    page("Notifications", &session, &body).into_response()
}

/// GET /notifications/read — mark all read, then back to the list.
pub async fn mark_read(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
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
    let events = state
        .store
        .list_repo_events(meta.id, 200)
        .await
        .unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\">\
         <h1>{}/{}</h1>\
         <h3>Audit log</h3>\
         <p class=\"muted\">Append-only record of security-relevant events.</p>\
         </div>",
        esc(&org),
        esc(&repo)
    );
    body.push_str(
        "<div class=\"card\">\
         <table>\
         <tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Detail</th></tr>",
    );
    if events.is_empty() {
        body.push_str(
            "<tr><td colspan=\"5\" class=\"muted\" style=\"padding:20px 8px\">\
             No events yet.</td></tr>",
        );
    }
    for e in events {
        body.push_str(&format!(
            "<tr>\
             <td class=\"muted\">{when}</td>\
             <td>{actor}</td>\
             <td><code>{action}</code></td>\
             <td>{target}</td>\
             <td class=\"muted\">{detail}</td>\
             </tr>",
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
/// Shared by both the full `search` page and the `search_results` HTMX partial.
async fn run_search(
    state: &AppState,
    session: &Option<rithub_core::User>,
    query: &str,
    org: &str,
    repo: &str,
) -> String {
    let _ = (org, repo); // reserved for scoped search in a future iteration
    if query.is_empty() {
        return String::new();
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
        "<div id=\"search-results\" \
              hx-target=\"this\" hx-swap=\"outerHTML\">\
         <h3>{count} result(s) for \u{201c}{q}\u{201d}</h3>",
        count = hits.len(),
        q = esc(query),
    );
    if hits.is_empty() {
        out.push_str("<p class=\"muted\">No matches found.</p>");
    }
    for hit in &hits {
        let (org_e, repo_e) = (esc(&hit.org), esc(&hit.repo));
        let href = match hit.kind.as_str() {
            "issue" => format!("/{org_e}/{repo_e}/issues/{}", hit.number),
            "pull_request" => format!("/{org_e}/{repo_e}/pulls/{}", hit.number),
            "commit" => format!("/{org_e}/{repo_e}/commit/{}", esc(&hit.oid)),
            _ => format!("/{org_e}/{repo_e}"),
        };
        let label = if hit.number > 0 {
            format!("{org_e}/{repo_e} #{} &mdash; {}", hit.number, esc(&hit.title))
        } else if !hit.path.is_empty() {
            format!("{org_e}/{repo_e} &mdash; {}", esc(&hit.path))
        } else {
            format!("{org_e}/{repo_e} &mdash; {}", esc(&hit.title))
        };
        let snippet = if hit.snippet.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"muted\" style=\"font-size:12px;margin-top:2px\">{}</div>",
                esc(&hit.snippet)
            )
        };
        out.push_str(&format!(
            "<div style=\"margin:10px 0;padding:8px 0;border-bottom:1px solid var(--line)\">\
             <span class=\"pill\">{kind}</span> \
             <a href=\"{href}\">{label}</a>\
             {snippet}\
             </div>",
            kind = esc(&hit.kind),
        ));
    }
    out.push_str("</div>");
    out
}

/// GET /search?q=... — global search page.
/// The search input carries HTMX attributes for live-as-you-type results that
/// update `#search-results` in-place while the form falls back gracefully.
pub async fn search(
    State(state): State<AppState>,
    Query(form): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let query = form.q.trim().to_string();

    // If this is an HTMX request on the search page (hx-boost navigation),
    // still return the full page so the URL and title update correctly.
    let results_html = run_search(&state, &session, &query, "", "").await;

    let results_block = if results_html.is_empty() {
        "<div id=\"search-results\"></div>".to_string()
    } else {
        results_html
    };

    let body = format!(
        "<div class=\"card\">\
         <h1>Search</h1>\
         <form method=\"get\" action=\"/search\">\
         <div class=\"row\">\
         <input name=\"q\" value=\"{q}\" placeholder=\"Search repositories, issues, PRs, commits\u{2026}\" \
                hx-get=\"/search/results\" \
                hx-trigger=\"keyup changed delay:350ms, search\" \
                hx-target=\"#search-results\" \
                hx-swap=\"outerHTML\" \
                hx-include=\"[name='q']\" \
                style=\"flex:1\">\
         <button class=\"btn primary\" type=\"submit\">Search</button>\
         </div></form>\
         </div>\
         {results_block}",
        q = esc(&query),
    );
    page("Search", &session, &body).into_response()
}

/// GET /search/results?q=... — HTMX partial: results `<div>` fragment only.
/// The response replaces `#search-results` in-place (see `hx-target` above).
/// Falls back gracefully: without htmx the input has no hx-get so this
/// endpoint is only ever called by htmx — but it is safe to call directly too.
pub async fn search_results(
    State(state): State<AppState>,
    Query(form): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let query = form.q.trim().to_string();

    let fragment = if query.is_empty() {
        "<div id=\"search-results\"></div>".to_string()
    } else {
        run_search(&state, &session, &query, "", "").await
    };

    // Always return a bare fragment regardless of HX-Request so this endpoint
    // can also be used by plain fetches (e.g. browser extensions, scripts).
    Html(fragment).into_response()
}

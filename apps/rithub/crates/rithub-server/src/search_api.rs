//! JSON API for full-text search and index administration.
//!
//! - `GET  /api/search` — ranked search across repositories, issues, pull
//!   requests, commit messages, and blob content, **scoped to what the caller
//!   may read**. Anonymous callers see public repositories only; an authenticated
//!   caller (session cookie or personal access token) additionally sees the
//!   private repositories they have access to.
//! - `POST /api/admin/search/reindex` — rebuild the whole index from Postgres +
//!   git. Runs in the background; returns immediately.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use forge_search::{DocKind, SearchScope};
use rithub_core::User;
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::search_index;
use crate::state::AppState;

/// Query parameters for `GET /api/search`.
#[derive(Debug, Deserialize)]
pub struct SearchParams {
    /// The search text.
    #[serde(default)]
    pub q: String,
    /// Optional kind filter: `repo`, `issue`, `pull_request`, `commit`, `blob`.
    pub kind: Option<String>,
    /// Optional repository filter as `org/name`.
    pub repo: Option<String>,
    /// Max results (clamped to a sane range).
    pub limit: Option<usize>,
}

/// Identify the caller from a session cookie or HTTP Basic PAT, if any.
async fn caller_user(state: &AppState, headers: &HeaderMap) -> Option<User> {
    match crate::authz::api_caller(state, headers).await {
        Ok(Some(caller)) if caller.has_scope("repo:read") => caller.user().cloned(),
        _ => None,
    }
}

/// `GET /api/search?q=...&kind=...&repo=org/name&limit=...`
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let query = params.q.trim().to_string();
    if query.is_empty() {
        return Ok(Json(json!({ "query": "", "hits": [] })));
    }
    let limit = params.limit.unwrap_or(50).clamp(1, 200);

    // Build the permission scope.
    let mut scope = SearchScope::anonymous();
    if let Some(user) = caller_user(&state, &headers).await {
        scope.visible_private_repo_ids =
            search_index::accessible_private_repo_ids(&state, user.id).await;
    }
    scope.kind = params.kind.as_deref().and_then(DocKind::parse);

    // Optional single-repo filter (`org/name`).
    if let Some(spec) = params.repo.as_deref() {
        if let Some((org, repo)) = spec.split_once('/') {
            if let Ok((meta, _)) = crate::authz::repo_for_read(&state, &headers, org, repo).await {
                scope.repo_id = Some(meta.id.to_string());
            } else {
                // Unknown repo → no results rather than an error.
                return Ok(Json(json!({ "query": query, "hits": [] })));
            }
        }
    }

    let hits = search_index::search(&state, &query, &scope, limit)
        .await
        .map_err(|e| AppError::internal(format!("search failed: {e}")))?;

    Ok(Json(json!({ "query": query, "hits": hits, "count": hits.len() })))
}

/// `POST /api/admin/search/reindex` — kick off a full rebuild in the background.
pub async fn reindex(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::authz::require_site_admin(&state, &headers).await?;
    tokio::spawn(async move {
        match search_index::reindex_all(&state).await {
            Ok(n) => tracing::info!(documents = n, "search reindex complete"),
            Err(e) => tracing::error!(error = %e, "search reindex failed"),
        }
    });
    Ok(Json(json!({ "status": "accepted", "detail": "reindex started" })))
}

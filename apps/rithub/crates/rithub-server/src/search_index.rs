//! Search indexing glue: translates forge domain rows and live git content into
//! [`forge_search::IndexDocument`]s and drives the active search backend.
//!
//! Two backends are supported, selected by `RITHUB_SEARCH_BACKEND`:
//!
//! - **`tantivy`** (default) — the on-disk full-text index owned by
//!   [`forge_search::SearchIndex`], carried in [`AppState::search`]. Blob and
//!   commit content, BM25 ranking.
//! - **`postgres`** — a `tsvector` mirror in the `search_documents` table
//!   (migration `0014`), queried with `websearch_to_tsquery`. Used when the
//!   native index is undesirable for a deployment.
//!
//! All incremental indexing is **best-effort**: a failure is logged and never
//! propagated into the originating write (creating an issue must not fail because
//! the search index hiccuped). Correctness is restored by `reindex`.
//!
//! Permission scoping is applied at query time: a hit for a **private** repo is
//! only returned when the caller can read that repo (see
//! [`accessible_private_repo_ids`]).

use forge_search::{IndexDocument, SearchError, SearchHit, SearchScope};
use rithub_core::{Repository as RepoMeta, Store};
use rithub_git::{ObjectId, RepoHost};
use uuid::Uuid;

use crate::state::AppState;

/// Which backend answers queries and receives incremental writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchBackend {
    Tantivy,
    Postgres,
}

impl SearchBackend {
    /// Read the backend from `RITHUB_SEARCH_BACKEND` (default `tantivy`).
    pub fn from_env() -> Self {
        match std::env::var("RITHUB_SEARCH_BACKEND")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "postgres" | "pg" | "tsvector" => SearchBackend::Postgres,
            _ => SearchBackend::Tantivy,
        }
    }
}

/// Newest N commits on the default branch to index (message search).
const MAX_INDEX_COMMITS: usize = 1_000;
/// Cap on blob files indexed per repository (content search).
const MAX_INDEX_BLOBS: usize = 2_000;
/// Skip indexing the content of any blob larger than this (still cheap to skip).
const MAX_BLOB_BYTES: usize = 512 * 1024;

// ---------------------------------------------------------------------------
// Incremental indexing (best-effort)
// ---------------------------------------------------------------------------

/// Index (or refresh) a repository's own metadata document.
pub async fn index_repo(state: &AppState, org: &str, repo: &RepoMeta) {
    let doc = IndexDocument::repo(
        &repo.id.to_string(),
        org,
        &repo.name,
        repo.visibility.as_str(),
        repo.description.as_deref().unwrap_or(""),
    );
    upsert_docs(state, vec![doc]).await;
}

/// Index (or refresh) one issue.
pub async fn index_issue(
    state: &AppState,
    org: &str,
    repo: &RepoMeta,
    number: i64,
    title: &str,
    body: &str,
) {
    let doc = IndexDocument::issue(
        &repo.id.to_string(),
        org,
        &repo.name,
        repo.visibility.as_str(),
        number,
        title,
        body,
    );
    upsert_docs(state, vec![doc]).await;
}

/// Index (or refresh) one pull request.
pub async fn index_pull(
    state: &AppState,
    org: &str,
    repo: &RepoMeta,
    number: i64,
    title: &str,
    body: &str,
) {
    let doc = IndexDocument::pull_request(
        &repo.id.to_string(),
        org,
        &repo.name,
        repo.visibility.as_str(),
        number,
        title,
        body,
    );
    upsert_docs(state, vec![doc]).await;
}

/// Index a repository's git content: recent commit messages and the text blobs
/// on its default branch. Bounded and best-effort; runs the (synchronous) git
/// reads on a blocking thread.
pub async fn index_repo_git(state: &AppState, org: &str, repo: &RepoMeta) {
    let host = state.host.clone();
    let org = org.to_string();
    let repo_name = repo.name.clone();
    let repo_id = repo.id.to_string();
    let visibility = repo.visibility.as_str().to_string();

    let docs = tokio::task::spawn_blocking(move || {
        collect_repo_git_docs(&host, &org, &repo_name, &repo_id, &visibility)
    })
    .await;

    match docs {
        Ok(docs) if !docs.is_empty() => upsert_docs(state, docs).await,
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "git index task panicked"),
    }
}

/// Remove every indexed document for a repository (on deletion). Provided for
/// the integrator to call from a future repo-deletion endpoint; the Postgres
/// backend also cleans up automatically via `ON DELETE CASCADE`.
#[allow(dead_code)]
pub async fn remove_repo(state: &AppState, repo_id: Uuid) {
    match SearchBackend::from_env() {
        SearchBackend::Tantivy => {
            if let Some(idx) = &state.search {
                let idx = idx.clone();
                let id = repo_id.to_string();
                let res = tokio::task::spawn_blocking(move || idx.remove_repo(&id)).await;
                log_index_result(res);
            }
        }
        SearchBackend::Postgres => {
            // `ON DELETE CASCADE` on search_documents.repo_id handles the common
            // case; this covers explicit index cleanup without a row delete.
            let _ = sqlx::query("DELETE FROM search_documents WHERE repo_id = $1")
                .bind(repo_id)
                .execute(state.store.pool())
                .await
                .map_err(|e| tracing::warn!(error = %e, "pg search cleanup failed"));
        }
    }
}

/// Dispatch an upsert of `docs` to the active backend. Best-effort.
async fn upsert_docs(state: &AppState, docs: Vec<IndexDocument>) {
    if docs.is_empty() {
        return;
    }
    match SearchBackend::from_env() {
        SearchBackend::Tantivy => {
            let Some(idx) = &state.search else {
                return;
            };
            let idx = idx.clone();
            let res = tokio::task::spawn_blocking(move || idx.upsert_many(&docs)).await;
            log_index_result(res);
        }
        SearchBackend::Postgres => {
            for d in &docs {
                if let Err(e) = pg_upsert(state, d).await {
                    tracing::warn!(error = %e, doc = %d.doc_id, "pg search upsert failed");
                }
            }
        }
    }
}

/// Log the outcome of a `spawn_blocking` index write without ever propagating it.
fn log_index_result(res: Result<Result<(), SearchError>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "search index write failed"),
        Err(e) => tracing::warn!(error = %e, "search index task panicked"),
    }
}

// ---------------------------------------------------------------------------
// Postgres backend helpers
// ---------------------------------------------------------------------------

async fn pg_upsert(state: &AppState, d: &IndexDocument) -> Result<(), sqlx::Error> {
    let repo_id = match Uuid::parse_str(&d.repo_id) {
        Ok(id) => id,
        Err(_) => return Ok(()), // non-UUID repo id: nothing to store
    };
    sqlx::query(
        "INSERT INTO search_documents \
           (doc_id, kind, repo_id, org, repo, visibility, number, oid, path, title, body, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now()) \
         ON CONFLICT (doc_id) DO UPDATE SET \
           kind=EXCLUDED.kind, org=EXCLUDED.org, repo=EXCLUDED.repo, \
           visibility=EXCLUDED.visibility, number=EXCLUDED.number, oid=EXCLUDED.oid, \
           path=EXCLUDED.path, title=EXCLUDED.title, body=EXCLUDED.body, updated_at=now()",
    )
    .bind(&d.doc_id)
    .bind(d.kind.as_str())
    .bind(repo_id)
    .bind(&d.org)
    .bind(&d.repo)
    .bind(&d.visibility)
    .bind(d.number as i32)
    .bind(&d.oid)
    .bind(&d.path)
    .bind(&d.title)
    .bind(&d.body)
    .execute(state.store.pool())
    .await
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Run a search against the active backend, scoped to `scope`.
pub async fn search(
    state: &AppState,
    query: &str,
    scope: &SearchScope,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    match SearchBackend::from_env() {
        SearchBackend::Tantivy => {
            let Some(idx) = &state.search else {
                return Ok(Vec::new());
            };
            let idx = idx.clone();
            let query = query.to_string();
            let scope = scope.clone();
            tokio::task::spawn_blocking(move || idx.search(&query, &scope, limit))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        }
        SearchBackend::Postgres => pg_search(state, query, scope, limit).await.map_err(|e| e.to_string()),
    }
}

async fn pg_search(
    state: &AppState,
    query: &str,
    scope: &SearchScope,
    limit: usize,
) -> Result<Vec<SearchHit>, sqlx::Error> {
    use sqlx::Row;

    // Visibility filter: public OR repo_id in the caller's accessible private set.
    let private: Vec<Uuid> = scope
        .visible_private_repo_ids
        .iter()
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    let kind = scope.kind.map(|k| k.as_str().to_string());
    let repo_filter: Option<Uuid> = scope
        .repo_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let rows = sqlx::query(
        "SELECT kind, org, repo, number, oid, path, title, body, \
                ts_rank_cd(tsv, q) AS rank \
         FROM search_documents, websearch_to_tsquery('simple', $1) AS q \
         WHERE tsv @@ q \
           AND (visibility = 'public' OR repo_id = ANY($2)) \
           AND ($3::text IS NULL OR kind = $3) \
           AND ($4::uuid IS NULL OR repo_id = $4) \
         ORDER BY rank DESC \
         LIMIT $5",
    )
    .bind(query)
    .bind(&private)
    .bind(kind)
    .bind(repo_filter)
    .bind(limit as i64)
    .fetch_all(state.store.pool())
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let body: String = r.get("body");
            SearchHit {
                kind: r.get("kind"),
                org: r.get("org"),
                repo: r.get("repo"),
                number: r.get::<i32, _>("number") as i64,
                oid: r.get("oid"),
                path: r.get("path"),
                title: r.get("title"),
                snippet: snippet(&body),
                score: r.get::<f32, _>("rank"),
            }
        })
        .collect())
}

fn snippet(body: &str) -> String {
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 240;
    if collapsed.len() <= MAX {
        return collapsed;
    }
    let mut end = MAX;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &collapsed[..end])
}

/// The private repositories `user_id` may read: repos in the user's orgs, repos
/// they collaborate on directly, and repos granted to teams they belong to.
/// Returned as repo-id strings matching the index's `repo_id` field.
pub async fn accessible_private_repo_ids(
    state: &AppState,
    user_id: Uuid,
) -> Vec<String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT r.id::text AS id FROM repositories r \
         WHERE r.visibility = 'private' AND ( \
             r.org_id IN (SELECT org_id FROM memberships WHERE user_id = $1) \
             OR r.id IN (SELECT repo_id FROM repo_collaborators WHERE user_id = $1) \
             OR r.id IN ( \
                 SELECT tra.repo_id FROM team_repo_access tra \
                 JOIN team_members tm ON tm.team_id = tra.team_id \
                 WHERE tm.user_id = $1 \
             ) \
         )",
    )
    .bind(user_id)
    .fetch_all(state.store.pool())
    .await;

    match rows {
        Ok(rows) => rows.iter().map(|r| r.get::<String, _>("id")).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "computing accessible repos failed — public only");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Full reindex
// ---------------------------------------------------------------------------

/// Rebuild the entire search index from Postgres + git. Returns the document
/// count. Safe to run online; it replaces prior content atomically for Tantivy
/// and refreshes every row for Postgres.
pub async fn reindex_all(state: &AppState) -> anyhow::Result<usize> {
    let docs = gather_all_docs(state).await?;
    let count = docs.len();

    match SearchBackend::from_env() {
        SearchBackend::Tantivy => {
            if let Some(idx) = &state.search {
                let idx = idx.clone();
                tokio::task::spawn_blocking(move || idx.reindex(docs))
                    .await
                    .map_err(|e| anyhow::anyhow!("reindex task panicked: {e}"))??;
            }
        }
        SearchBackend::Postgres => {
            sqlx::query("TRUNCATE search_documents")
                .execute(state.store.pool())
                .await?;
            for d in &docs {
                pg_upsert(state, d).await?;
            }
        }
    }

    // Record the checkpoint (singleton row id = 1).
    let backend = match SearchBackend::from_env() {
        SearchBackend::Tantivy => "tantivy",
        SearchBackend::Postgres => "postgres",
    };
    sqlx::query(
        "INSERT INTO search_index_meta (id, backend, last_reindex_at, document_count, updated_at) \
         VALUES (1, $1, now(), $2, now()) \
         ON CONFLICT (id) DO UPDATE SET \
           backend = EXCLUDED.backend, last_reindex_at = now(), \
           document_count = EXCLUDED.document_count, updated_at = now()",
    )
    .bind(backend)
    .bind(count as i64)
    .execute(state.store.pool())
    .await?;

    Ok(count)
}

/// Collect every indexable document across all orgs and repos.
async fn gather_all_docs(state: &AppState) -> anyhow::Result<Vec<IndexDocument>> {
    use sqlx::Row;

    let mut docs = Vec::new();
    let orgs = state.store.list_orgs().await?;
    for org in orgs {
        let repos = state.store.list_repos(&org).await?;
        for repo in repos {
            let repo_id = repo.id.to_string();
            let vis = repo.visibility.as_str().to_string();

            // Repository metadata.
            docs.push(IndexDocument::repo(
                &repo_id,
                &org.name,
                &repo.name,
                &vis,
                repo.description.as_deref().unwrap_or(""),
            ));

            // Issues.
            let issue_rows = sqlx::query(
                "SELECT number, title, COALESCE(body,'') AS body FROM issues WHERE repo_id = $1",
            )
            .bind(repo.id)
            .fetch_all(state.store.pool())
            .await?;
            for r in &issue_rows {
                docs.push(IndexDocument::issue(
                    &repo_id,
                    &org.name,
                    &repo.name,
                    &vis,
                    r.get::<i32, _>("number") as i64,
                    &r.get::<String, _>("title"),
                    &r.get::<String, _>("body"),
                ));
            }

            // Pull requests.
            let pr_rows = sqlx::query(
                "SELECT number, title, COALESCE(body,'') AS body FROM pull_requests WHERE repo_id = $1",
            )
            .bind(repo.id)
            .fetch_all(state.store.pool())
            .await?;
            for r in &pr_rows {
                docs.push(IndexDocument::pull_request(
                    &repo_id,
                    &org.name,
                    &repo.name,
                    &vis,
                    r.get::<i32, _>("number") as i64,
                    &r.get::<String, _>("title"),
                    &r.get::<String, _>("body"),
                ));
            }

            // Git content (commits + blobs), off the async runtime threads.
            let host = state.host.clone();
            let org_name = org.name.clone();
            let repo_name = repo.name.clone();
            let rid = repo_id.clone();
            let vis2 = vis.clone();
            let git_docs = tokio::task::spawn_blocking(move || {
                collect_repo_git_docs(&host, &org_name, &repo_name, &rid, &vis2)
            })
            .await
            .unwrap_or_default();
            docs.extend(git_docs);
        }
    }
    Ok(docs)
}

// ---------------------------------------------------------------------------
// Git document collection (synchronous; call via spawn_blocking)
// ---------------------------------------------------------------------------

/// Build index documents for a repository's default-branch commits and text
/// blobs. Bounded by [`MAX_INDEX_COMMITS`] / [`MAX_INDEX_BLOBS`]. Returns an
/// empty vec for empty/unreadable repositories.
fn collect_repo_git_docs(
    host: &RepoHost,
    org: &str,
    repo: &str,
    repo_id: &str,
    visibility: &str,
) -> Vec<IndexDocument> {
    let mut docs = Vec::new();

    let default_branch = match host.default_branch(org, repo) {
        Ok(b) => b,
        Err(_) => return docs,
    };
    let commit = match host.resolve_commit(org, repo, &default_branch) {
        Ok(c) => c,
        Err(_) => return docs, // empty repo or bad ref
    };

    // Commit messages.
    if let Ok(history) = host.history(org, repo, commit, MAX_INDEX_COMMITS) {
        for c in history {
            docs.push(IndexDocument::commit(
                repo_id,
                org,
                repo,
                visibility,
                &c.oid,
                &c.summary,
                &c.message,
            ));
        }
    }

    // Blob content (bounded tree walk).
    let mut budget = MAX_INDEX_BLOBS;
    collect_blob_docs(
        host, org, repo, repo_id, visibility, commit, "", 0, &mut budget, &mut docs,
    );

    docs
}

/// Depth cap mirrors `rithub-git`'s archive walk.
const MAX_TREE_DEPTH: usize = 64;

#[allow(clippy::too_many_arguments)]
fn collect_blob_docs(
    host: &RepoHost,
    org: &str,
    repo: &str,
    repo_id: &str,
    visibility: &str,
    commit: ObjectId,
    path: &str,
    depth: usize,
    budget: &mut usize,
    out: &mut Vec<IndexDocument>,
) {
    if depth > MAX_TREE_DEPTH || *budget == 0 {
        return;
    }
    let items = match host.list_tree(org, repo, commit, path) {
        Ok(items) => items,
        Err(_) => return,
    };
    for item in items {
        if *budget == 0 {
            return;
        }
        let child = if path.is_empty() {
            item.name.clone()
        } else {
            format!("{path}/{}", item.name)
        };
        if item.is_dir {
            collect_blob_docs(
                host, org, repo, repo_id, visibility, commit, &child, depth + 1, budget, out,
            );
        } else if let Ok(content) = host.read_blob_at(org, repo, commit, &child) {
            // Only index reasonably-sized text blobs.
            if content.binary || content.bytes.len() > MAX_BLOB_BYTES {
                continue;
            }
            let text = String::from_utf8_lossy(&content.bytes);
            out.push(IndexDocument::blob(
                repo_id,
                org,
                repo,
                visibility,
                &child,
                &item.oid,
                &text,
            ));
            *budget -= 1;
        }
    }
}

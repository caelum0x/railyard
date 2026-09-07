//! Basic search across repositories, issues, and pull requests using
//! case-insensitive `ILIKE` matching. A `_`/`%` in the query is escaped so it is
//! treated as a literal, not a wildcard.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::Serialize;
use sqlx::Row;

/// A single search hit.
#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    /// `"repo"`, `"issue"`, or `"pull_request"`.
    pub kind: String,
    pub org: String,
    pub repo: String,
    /// Issue/PR number (0 for a repo hit).
    pub number: i32,
    pub title: String,
}

/// Search persistence.
#[allow(async_fn_in_trait)]
pub trait SearchStore {
    /// Global search across repos + issues + PRs the query matches.
    async fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>>;
}

/// Escape ILIKE wildcards so user input matches literally.
fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

impl SearchStore for PgStore {
    async fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let pattern = like_pattern(query);
        let mut hits = Vec::new();

        // Repositories (by name or description).
        let repo_rows = sqlx::query(
            "SELECT o.name AS org, r.name AS repo, COALESCE(r.description,'') AS descr \
             FROM repositories r JOIN organizations o ON o.id = r.org_id \
             WHERE r.name ILIKE $1 ESCAPE '\\' OR r.description ILIKE $1 ESCAPE '\\' \
             ORDER BY r.created_at DESC LIMIT $2",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        for row in &repo_rows {
            hits.push(SearchHit {
                kind: "repo".into(),
                org: row.get("org"),
                repo: row.get("repo"),
                number: 0,
                title: row.get("descr"),
            });
        }

        // Issues (by title or body).
        let issue_rows = sqlx::query(
            "SELECT o.name AS org, r.name AS repo, i.number, i.title FROM issues i \
             JOIN repositories r ON r.id = i.repo_id JOIN organizations o ON o.id = r.org_id \
             WHERE i.title ILIKE $1 ESCAPE '\\' OR i.body ILIKE $1 ESCAPE '\\' \
             ORDER BY i.created_at DESC LIMIT $2",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        for row in &issue_rows {
            hits.push(SearchHit {
                kind: "issue".into(),
                org: row.get("org"),
                repo: row.get("repo"),
                number: row.get("number"),
                title: row.get("title"),
            });
        }

        // Pull requests (by title or body).
        let pr_rows = sqlx::query(
            "SELECT o.name AS org, r.name AS repo, p.number, p.title FROM pull_requests p \
             JOIN repositories r ON r.id = p.repo_id JOIN organizations o ON o.id = r.org_id \
             WHERE p.title ILIKE $1 ESCAPE '\\' OR p.body ILIKE $1 ESCAPE '\\' \
             ORDER BY p.created_at DESC LIMIT $2",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        for row in &pr_rows {
            hits.push(SearchHit {
                kind: "pull_request".into(),
                org: row.get("org"),
                repo: row.get("repo"),
                number: row.get("number"),
                title: row.get("title"),
            });
        }

        Ok(hits)
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("search", "*", e)
}

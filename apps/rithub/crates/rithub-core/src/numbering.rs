//! Per-repository sequential number allocation for issues and pull requests.
//!
//! Numbers must be gap-free and stable per repo, so allocation is serialized by
//! taking a row lock on the owning repository inside the caller's transaction:
//! `SELECT ... FOR UPDATE` blocks concurrent allocators for the same repo, then
//! we read `MAX(number)+1` from the target table. This avoids a unique-violation
//! race without a global sequence.

use crate::error::{CoreError, Result};
use uuid::Uuid;

/// Allocate the next number for `table` (`"issues"` or `"pull_requests"`) within
/// `tx`, holding a row lock on the repository. `table` is a fixed internal
/// constant — never user input — so interpolating it carries no injection risk.
pub(crate) async fn next_repo_number(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo_id: Uuid,
    table: &'static str,
) -> Result<i32> {
    debug_assert!(matches!(table, "issues" | "pull_requests"));
    // Serialize allocation for this repo.
    sqlx::query("SELECT id FROM repositories WHERE id = $1 FOR UPDATE")
        .bind(repo_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| CoreError::from_sqlx("repository", &repo_id.to_string(), e))?;

    let next: i32 = sqlx::query_scalar(&format!(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM {table} WHERE repo_id = $1"
    ))
    .bind(repo_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| CoreError::from_sqlx(table, &repo_id.to_string(), e))?;
    Ok(next)
}

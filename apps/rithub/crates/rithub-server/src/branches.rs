//! Branch management: list branches (with tips and protection), delete a
//! branch, and remove a branch-protection rule. Shared by the JSON API and the
//! web UI (repository settings + the "Delete branch" button on merged PRs).
//!
//! Deleting a branch is refused for the default branch, for a protected branch
//! (remove the rule first), and for a branch that is the base or head of an
//! open pull request. A deletion fires the `push` webhook (with the ref marked deleted)
//! and is recorded in the audit log.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{PrState, PullRequestStore, RepoRole, Repository, ReviewStore, User};
use serde::Serialize;

use crate::authz::{repo_for_read, repo_for_role};
use crate::collab_svc::{enqueue_audit, enqueue_webhook};
use crate::error::AppError;
use crate::state::AppState;

/// One branch as returned by the API.
#[derive(Debug, Serialize)]
pub struct BranchInfo {
    pub name: String,
    pub sha: Option<String>,
    pub default: bool,
    pub protected: bool,
}

/// Every branch of `org/repo` with its tip and protection flag.
pub async fn list_branches(
    state: &AppState,
    org: &str,
    meta: &Repository,
) -> Result<Vec<BranchInfo>, AppError> {
    let names = state.host.branches(org, &meta.name)?;
    let default = state
        .host
        .default_branch(org, &meta.name)
        .unwrap_or_else(|_| meta.default_branch.clone());
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let sha = state
            .host
            .resolve_branch(org, &meta.name, &name)
            .ok()
            .map(|oid| oid.to_hex());
        let protected = state
            .store
            .get_branch_protection(meta.id, &name)
            .await?
            .is_some();
        out.push(BranchInfo { default: name == default, name, sha, protected });
    }
    Ok(out)
}

/// Remove the protection rule on `branch`. 404 when the branch had no rule.
pub async fn remove_protection(
    state: &AppState,
    meta: &Repository,
    actor: &User,
    branch: &str,
) -> Result<(), AppError> {
    let branch = branch.trim();
    if !state.store.delete_branch_protection(meta.id, branch).await? {
        return Err(AppError::not_found(format!("branch {branch} is not protected")));
    }
    enqueue_audit(state, Some(meta.id), Some(actor), "protection.removed", Some(branch), None);
    Ok(())
}

/// Delete `branch` from the repository after the safety checks above.
pub async fn delete_branch(
    state: &AppState,
    org: &str,
    meta: &Repository,
    actor: &User,
    branch: &str,
) -> Result<(), AppError> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err(AppError::bad_request("branch is required"));
    }
    let default = state
        .host
        .default_branch(org, &meta.name)
        .unwrap_or_else(|_| meta.default_branch.clone());
    if branch == default {
        return Err(AppError::conflict(
            "the default branch cannot be deleted; change the default branch first",
        ));
    }
    if state.store.get_branch_protection(meta.id, branch).await?.is_some() {
        return Err(AppError::conflict(format!(
            "branch {branch} is protected; remove its protection rule first"
        )));
    }
    let open = state.store.list_prs(meta.id, Some(PrState::Open)).await?;
    if let Some(pr) = open.iter().find(|pr| pr.base_branch == branch) {
        return Err(AppError::conflict(format!(
            "branch {branch} is the base of open pull request #{}",
            pr.number
        )));
    }
    let same_repo_head = |pr: &&rithub_core::PullRequest| {
        pr.head_branch == branch && pr.head_repo_id.is_none_or(|id| id == meta.id)
    };
    if let Some(pr) = open.iter().find(same_repo_head) {
        return Err(AppError::conflict(format!(
            "branch {branch} is the head of open pull request #{}; close or merge it first",
            pr.number
        )));
    }

    let tip = state.host.delete_branch(org, &meta.name, branch)?;
    let reference = format!("refs/heads/{branch}");
    enqueue_audit(
        state,
        Some(meta.id),
        Some(actor),
        "branch.deleted",
        Some(&reference),
        Some(&tip.to_hex()),
    );
    enqueue_webhook(
        state,
        meta.id,
        "push",
        serde_json::json!({
            "event": "push",
            "repo": meta.name,
            "pusher": actor.username,
            "refs": [reference],
            "deleted": true,
            "before": tip.to_hex(),
        }),
    );
    let st = state.clone();
    let org_s = org.to_string();
    let meta_c = meta.clone();
    tokio::spawn(async move {
        crate::search_index::index_repo_git(&st, &org_s, &meta_c).await;
    });
    Ok(())
}

// --- API handlers --------------------------------------------------------------

/// `GET /api/orgs/{org}/repos/{repo}/branches`
pub async fn api_list_branches(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Vec<BranchInfo>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    Ok(Json(list_branches(&state, &org, &meta).await?))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/branches/{branch}` (Write).
pub async fn api_delete_branch(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    delete_branch(&state, &org, &meta, &user, &branch).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/orgs/{org}/repos/{repo}/branches/{branch}/protection` (Admin).
pub async fn api_delete_protection(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    remove_protection(&state, &meta, &user, &branch).await?;
    Ok(StatusCode::NO_CONTENT)
}

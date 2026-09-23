//! Collaboration JSON API: issues, pull requests (diff, mergeability, merge),
//! reviews (with a protected-branch approval gate), branch protection, and
//! webhooks.
//!
//! Every handler authenticates the caller ([`crate::authz`]) and attributes
//! authorship to the **authenticated** user — never to a username supplied in
//! the request body. Mutations run through [`crate::collab_svc`], which the web
//! UI shares, so side effects (webhooks, notifications, email, audit, CI,
//! search indexing) are identical on both surfaces.
//!
//! ## Permissions
//! | Action | Requirement |
//! |--------|-------------|
//! | read issues/PRs/diffs/reviews | read access to the repo |
//! | open issue/PR, comment, review (comment / request changes) | signed in + read access |
//! | approve a PR | Write (and not the PR author) |
//! | edit title/body, close/reopen | the author, or Write |
//! | labels, assignees, milestones, merge | Write |
//! | branch protection, webhooks | Admin |

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{
    DirectoryStore, Issue, IssueComment, IssueState, IssueStore, NewReviewComment, NewWebhook,
    PrComment, PrState, PullRequest, PullRequestStore, RepoRole, Review, ReviewState, ReviewStore,
    Webhook, WebhookDelivery, WebhookPatch, WebhookStore,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::authz::{repo_for_participant, repo_for_read, repo_for_role};
use crate::collab_svc::{self as svc, IssueDetail, IssuePatch, LabelSpec, PrInput, PrPatch, RepoRef};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StateFilter {
    #[serde(default)]
    pub state: Option<String>,
}

// --- issues -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateIssue {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
}

pub async fn create_issue(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CreateIssue>,
) -> Result<Json<Issue>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    Ok(Json(svc::create_issue(&state, &r, &user, &input.title, input.body).await?))
}

pub async fn list_issues(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Query(filter): Query<StateFilter>,
) -> Result<Json<Vec<Issue>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let state_filter = filter.state.as_deref().map(IssueState::parse);
    Ok(Json(state.store.list_issues(meta.id, state_filter).await?))
}

pub async fn get_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<IssueDetail>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    Ok(Json(svc::issue_detail(&state, &meta, number).await?))
}

/// Shared close/reopen body: author or Write may change state.
async fn change_issue_state(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
    number: i32,
    target: IssueState,
) -> Result<Issue, AppError> {
    let (meta, user) = repo_for_participant(state, headers, org, repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    if !svc::is_author_or_role(state, &user, &meta, issue.author_id, RepoRole::Write).await {
        return Err(AppError::forbidden("only the author or a writer can change issue state"));
    }
    let r = RepoRef { org, repo, meta: &meta };
    svc::set_issue_state(state, &r, number, &user, target).await
}

pub async fn close_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<Issue>, AppError> {
    Ok(Json(
        change_issue_state(&state, &headers, &org, &repo, number, IssueState::Closed).await?,
    ))
}

pub async fn reopen_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<Issue>, AppError> {
    Ok(Json(
        change_issue_state(&state, &headers, &org, &repo, number, IssueState::Open).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct CommentBody {
    pub body: String,
}

pub async fn comment_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<CommentBody>,
) -> Result<Json<IssueComment>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    Ok(Json(svc::comment_issue(&state, &r, number, &user, &input.body).await?))
}

pub async fn list_issue_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<Vec<IssueComment>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    Ok(Json(state.store.list_issue_comments(issue.id).await?))
}

#[derive(Debug, Deserialize)]
pub struct LabelInput {
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// `POST .../issues/{n}/labels` — add one label (Write).
pub async fn label_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<LabelInput>,
) -> Result<Json<IssueDetail>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let mut labels: Vec<LabelSpec> = state
        .store
        .list_issue_labels(issue.id)
        .await?
        .into_iter()
        .map(|l| LabelSpec { name: l.name, color: Some(l.color) })
        .collect();
    if !labels.iter().any(|l| l.name == input.label.trim()) {
        labels.push(LabelSpec { name: input.label, color: input.color });
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let patch = IssuePatch { labels: Some(labels), ..IssuePatch::default() };
    Ok(Json(svc::update_issue(&state, &r, number, &user, patch).await?))
}

#[derive(Debug, Deserialize)]
pub struct AssigneeInput {
    pub username: String,
}

/// `POST .../issues/{n}/assignees` — add one assignee (Write).
pub async fn assign_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<AssigneeInput>,
) -> Result<Json<IssueDetail>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let mut assignees: Vec<String> = state
        .store
        .list_issue_assignees(issue.id)
        .await?
        .into_iter()
        .map(|u| u.username)
        .collect();
    if !assignees.iter().any(|a| a == input.username.trim()) {
        assignees.push(input.username.trim().to_string());
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let patch = IssuePatch { assignees: Some(assignees), ..IssuePatch::default() };
    Ok(Json(svc::update_issue(&state, &r, number, &user, patch).await?))
}

/// Update request for an issue. All fields are optional.
#[derive(Debug, Deserialize)]
pub struct UpdateIssueInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// `"open"` or `"closed"`.
    #[serde(default)]
    pub state: Option<String>,
    /// Milestone number to assign, or `0` to clear.
    #[serde(default)]
    pub milestone: Option<i32>,
    /// Replaces the full assignee list when provided.
    #[serde(default)]
    pub assignees: Option<Vec<String>>,
    /// Replaces the full label list when provided.
    #[serde(default)]
    pub labels: Option<Vec<LabelInput>>,
}

pub async fn update_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<UpdateIssueInput>,
) -> Result<Json<IssueDetail>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let triage = input.milestone.is_some() || input.assignees.is_some() || input.labels.is_some();
    let allowed = if triage {
        matches!(crate::authz::user_repo_role(&state, &user, &meta).await, Some(r) if r.can_write())
    } else {
        svc::is_author_or_role(&state, &user, &meta, issue.author_id, RepoRole::Write).await
    };
    if !allowed {
        return Err(AppError::forbidden(if triage {
            "labels, assignees and milestones require write access"
        } else {
            "only the author or a writer can edit this issue"
        }));
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let patch = IssuePatch {
        title: input.title,
        body: input.body,
        state: input.state.as_deref().map(IssueState::parse),
        milestone: input.milestone,
        assignees: input.assignees,
        labels: input.labels.map(|ls| {
            ls.into_iter()
                .map(|l| LabelSpec { name: l.label, color: l.color })
                .collect()
        }),
    };
    Ok(Json(svc::update_issue(&state, &r, number, &user, patch).await?))
}

// --- pull requests ----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePr {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub head: String,
    pub base: String,
    /// For a **cross-repo** PR: the source `org/repo` the head branch lives in.
    #[serde(default)]
    pub head_repo: Option<String>,
}

pub async fn create_pr(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CreatePr>,
) -> Result<Json<PullRequest>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let pr = svc::create_pr(
        &state,
        &r,
        &user,
        PrInput {
            title: input.title,
            body: input.body,
            head: input.head,
            base: input.base,
            head_repo: input.head_repo,
        },
    )
    .await?;
    Ok(Json(pr))
}

pub async fn list_prs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Query(filter): Query<StateFilter>,
) -> Result<Json<Vec<PullRequest>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let state_filter = filter.state.as_deref().map(PrState::parse);
    Ok(Json(state.store.list_prs(meta.id, state_filter).await?))
}

#[derive(Debug, Serialize)]
pub struct PrDetail {
    #[serde(flatten)]
    pub pr: PullRequest,
    pub mergeable: Option<bool>,
    pub merge_status: Option<String>,
    pub approvals: i64,
}

pub async fn get_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<PrDetail>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let analysis = svc::current_mergeability(&state, &org, &repo, &pr).ok();
    let approvals = state
        .store
        .count_current_approvals(pr.id, pr.author_id)
        .await
        .unwrap_or(0);
    Ok(Json(PrDetail {
        pr,
        mergeable: analysis.as_ref().map(|a| a.mergeable),
        merge_status: analysis.map(|a| a.status),
        approvals,
    }))
}

pub async fn pr_mergeability(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let m = svc::current_mergeability(&state, &org, &repo, &pr)?;
    Ok(Json(serde_json::json!({
        "status": m.status,
        "mergeable": m.mergeable,
        "base_sha": m.base_sha,
        "head_sha": m.head_sha,
        "merge_base": m.merge_base,
    })))
}

#[derive(Debug, Serialize)]
pub struct DiffDto {
    pub additions: usize,
    pub deletions: usize,
    pub files: Vec<FileDiffDto>,
}

#[derive(Debug, Serialize)]
pub struct FileDiffDto {
    pub path: String,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
    pub patch: String,
}

impl From<rithub_git::DiffResult> for DiffDto {
    fn from(d: rithub_git::DiffResult) -> Self {
        DiffDto {
            additions: d.additions,
            deletions: d.deletions,
            files: d
                .files
                .into_iter()
                .map(|f| FileDiffDto {
                    path: f.path,
                    status: f.status.to_string(),
                    additions: f.additions,
                    deletions: f.deletions,
                    binary: f.binary,
                    patch: f.patch,
                })
                .collect(),
        }
    }
}

pub async fn pr_diff(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<DiffDto>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    Ok(Json(DiffDto::from(svc::pr_diff(&state, &org, &repo, &pr)?)))
}

pub async fn pr_commits(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let base = state
        .host
        .resolve_branch(&org, &repo, &pr.base_branch)
        .map_err(AppError::from)?;
    let head = svc::resolve_pr_head(&state, &org, &repo, &pr)?;
    let commits = state
        .host
        .commit_list(&org, &repo, base, head)
        .map_err(AppError::from)?;
    let json: Vec<_> = commits
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "oid": c.oid, "summary": c.summary,
                "author_name": c.author_name, "author_email": c.author_email,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "commits": json })))
}

pub async fn comment_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<CommentBody>,
) -> Result<Json<PrComment>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    Ok(Json(svc::comment_pr(&state, &r, number, &user, &input.body).await?))
}

pub async fn list_pr_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<Vec<PrComment>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    Ok(Json(state.store.list_pr_comments(pr.id).await?))
}

/// Shared close/reopen body: author or Write.
async fn change_pr_state(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
    number: i32,
    open: bool,
) -> Result<PullRequest, AppError> {
    let (meta, user) = repo_for_participant(state, headers, org, repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    if !svc::is_author_or_role(state, &user, &meta, pr.author_id, RepoRole::Write).await {
        return Err(AppError::forbidden("only the author or a writer can change PR state"));
    }
    let r = RepoRef { org, repo, meta: &meta };
    svc::set_pr_state(state, &r, number, &user, open).await
}

pub async fn close_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<PullRequest>, AppError> {
    Ok(Json(change_pr_state(&state, &headers, &org, &repo, number, false).await?))
}

pub async fn reopen_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<PullRequest>, AppError> {
    Ok(Json(change_pr_state(&state, &headers, &org, &repo, number, true).await?))
}

/// Update request for a pull request. All fields optional.
#[derive(Debug, Deserialize)]
pub struct UpdatePrInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    /// `"open"` (reopen) or `"closed"` (close).
    #[serde(default)]
    pub state: Option<String>,
    /// Milestone number to assign, or `0` to clear.
    #[serde(default)]
    pub milestone: Option<i32>,
}

pub async fn update_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<UpdatePrInput>,
) -> Result<Json<PullRequest>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let allowed = if input.milestone.is_some() {
        matches!(crate::authz::user_repo_role(&state, &user, &meta).await, Some(r) if r.can_write())
    } else {
        svc::is_author_or_role(&state, &user, &meta, pr.author_id, RepoRole::Write).await
    };
    if !allowed {
        return Err(AppError::forbidden("only the author or a writer can edit this pull request"));
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let mut updated = svc::update_pr(
        &state,
        &r,
        number,
        PrPatch {
            title: input.title,
            body: input.body,
            base: input.base,
            milestone: input.milestone,
        },
    )
    .await?;
    match input.state.as_deref() {
        Some("closed") if updated.state == PrState::Open => {
            updated = svc::set_pr_state(&state, &r, number, &user, false).await?;
        }
        Some("open") if updated.state == PrState::Closed => {
            updated = svc::set_pr_state(&state, &r, number, &user, true).await?;
        }
        _ => {}
    }
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
pub struct MergeInput {
    #[serde(default)]
    pub message: Option<String>,
    /// `"merge"` (default), `"squash"`, or `"rebase"`.
    #[serde(default)]
    pub strategy: Option<String>,
}

pub async fn merge_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<MergeInput>,
) -> Result<Json<svc::MergeOutcome>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let outcome = svc::merge_pr(
        &state,
        &r,
        number,
        &user,
        input.strategy.as_deref(),
        input.message,
    )
    .await?;
    Ok(Json(outcome))
}

// --- reviews ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateReview {
    pub state: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub comments: Vec<NewReviewComment>,
}

pub async fn create_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<CreateReview>,
) -> Result<Json<Review>, AppError> {
    let (meta, user) = repo_for_participant(&state, &headers, &org, &repo).await?;
    let review_state = ReviewState::parse(&input.state).ok_or_else(|| {
        AppError::bad_request("state must be approved|changes_requested|commented")
    })?;
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let review = svc::create_review(
        &state,
        &r,
        number,
        &user,
        review_state,
        input.body,
        &input.comments,
    )
    .await?;
    Ok(Json(review))
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let reviews = state.store.list_reviews(pr.id).await?;
    let comments = state.store.list_review_comments(pr.id).await?;
    let approvals = state
        .store
        .count_current_approvals(pr.id, pr.author_id)
        .await?;
    Ok(Json(serde_json::json!({
        "reviews": reviews,
        "comments": comments,
        "current_approvals": approvals,
    })))
}

// --- branch protection ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ProtectionInput {
    #[serde(default = "one")]
    pub required_approvals: i32,
    #[serde(default = "yes")]
    pub block_direct_push: bool,
    #[serde(default)]
    pub required_checks: Vec<String>,
}

fn one() -> i32 {
    1
}
fn yes() -> bool {
    true
}

/// Persist a branch-protection rule (shared with the web settings page).
pub async fn apply_protection(
    state: &AppState,
    meta: &rithub_core::Repository,
    actor: &rithub_core::User,
    branch: &str,
    input: &ProtectionInput,
) -> Result<rithub_core::BranchProtection, AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::bad_request("branch is required"));
    }
    if !(0..=20).contains(&input.required_approvals) {
        return Err(AppError::bad_request("required_approvals must be between 0 and 20"));
    }
    let checks: Vec<String> = input
        .required_checks
        .iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    let protection = state
        .store
        .set_branch_protection(
            meta.id,
            branch.trim(),
            input.required_approvals,
            input.block_direct_push,
            &checks,
        )
        .await?;
    svc::enqueue_audit(
        state,
        Some(meta.id),
        Some(actor),
        "protection.changed",
        Some(branch),
        Some(&format!(
            "approvals={}, block_push={}, checks=[{}]",
            input.required_approvals,
            input.block_direct_push,
            checks.join(",")
        )),
    );
    Ok(protection)
}

pub async fn set_protection(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(input): Json<ProtectionInput>,
) -> Result<Json<rithub_core::BranchProtection>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    Ok(Json(apply_protection(&state, &meta, &user, &branch, &input).await?))
}

pub async fn get_protection(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let protection = state.store.get_branch_protection(meta.id, &branch).await?;
    Ok(Json(serde_json::json!({ "protection": protection })))
}

// --- webhooks ---------------------------------------------------------------

/// Validate + create a webhook (shared with the web settings page).
pub async fn register_hook(
    state: &AppState,
    meta: &rithub_core::Repository,
    actor: &rithub_core::User,
    input: &NewWebhook,
) -> Result<Webhook, AppError> {
    crate::webhooks::validate_target(&input.url)
        .await
        .map_err(|reason| AppError::bad_request(format!("invalid webhook URL: {reason}")))?;
    if input.secret.trim().len() < 8 {
        return Err(AppError::bad_request("webhook secret must be at least 8 characters"));
    }
    let hook = state.store.create_webhook(meta.id, input).await?;
    svc::enqueue_audit(
        state,
        Some(meta.id),
        Some(actor),
        "webhook.created",
        Some(&hook.url),
        Some(&format!("events=[{}]", hook.events.join(","))),
    );
    Ok(hook)
}

pub async fn create_hook(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<NewWebhook>,
) -> Result<Json<Webhook>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    Ok(Json(register_hook(&state, &meta, &user, &input).await?))
}

pub async fn list_hooks(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Vec<Webhook>>, AppError> {
    let (meta, _) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    Ok(Json(state.store.list_webhooks(meta.id).await?))
}

pub async fn get_hook(
    State(state): State<AppState>,
    Path((org, repo, hook_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    let hook = state.store.get_webhook(meta.id, hook_id).await?;
    let deliveries: Vec<WebhookDelivery> = state.store.list_webhook_deliveries(hook.id, 20).await?;
    Ok(Json(serde_json::json!({ "hook": hook, "deliveries": deliveries })))
}

/// Validate + apply a webhook patch (shared with the web settings page).
pub async fn patch_hook(
    state: &AppState,
    meta: &rithub_core::Repository,
    actor: &rithub_core::User,
    hook_id: Uuid,
    patch: &WebhookPatch,
) -> Result<Webhook, AppError> {
    if let Some(url) = &patch.url {
        crate::webhooks::validate_target(url)
            .await
            .map_err(|reason| AppError::bad_request(format!("invalid webhook URL: {reason}")))?;
    }
    if patch.secret.as_deref().is_some_and(|s| s.trim().len() < 8) {
        return Err(AppError::bad_request("webhook secret must be at least 8 characters"));
    }
    let hook = state.store.update_webhook(meta.id, hook_id, patch).await?;
    svc::enqueue_audit(
        state,
        Some(meta.id),
        Some(actor),
        "webhook.updated",
        Some(&hook.url),
        Some(&format!("active={}", hook.active)),
    );
    Ok(hook)
}

pub async fn update_hook(
    State(state): State<AppState>,
    Path((org, repo, hook_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    Json(patch): Json<WebhookPatch>,
) -> Result<Json<Webhook>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    Ok(Json(patch_hook(&state, &meta, &user, hook_id, &patch).await?))
}

pub async fn delete_hook(
    State(state): State<AppState>,
    Path((org, repo, hook_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    let hook = state.store.get_webhook(meta.id, hook_id).await?;
    state.store.delete_webhook(meta.id, hook_id).await?;
    svc::enqueue_audit(&state, Some(meta.id), Some(&user), "webhook.deleted", Some(&hook.url), None);
    Ok(StatusCode::NO_CONTENT)
}

// --- labels -------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LabelBody {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LabelPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// `GET /api/orgs/{org}/repos/{repo}/labels`
pub async fn list_labels(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Vec<rithub_core::Label>>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    Ok(Json(state.store.list_labels(meta.id).await?))
}

/// `POST /api/orgs/{org}/repos/{repo}/labels` (Write)
pub async fn create_label(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<LabelBody>,
) -> Result<Json<rithub_core::Label>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    Ok(Json(
        svc::create_label(&state, &meta, &user, &input.name, input.color.as_deref()).await?,
    ))
}

/// `PATCH /api/orgs/{org}/repos/{repo}/labels/{name}` (Write)
pub async fn update_label(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(input): Json<LabelPatch>,
) -> Result<Json<rithub_core::Label>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let label = svc::label_by_name(&state, &meta, &name).await?;
    Ok(Json(
        svc::edit_label(
            &state,
            &meta,
            &user,
            label.id,
            input.name.as_deref(),
            input.color.as_deref(),
        )
        .await?,
    ))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/labels/{name}` (Write)
pub async fn delete_label(
    State(state): State<AppState>,
    Path((org, repo, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let label = svc::label_by_name(&state, &meta, &name).await?;
    svc::delete_label(&state, &meta, &user, label.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

//! Collaboration API: issues, pull requests (diff, mergeability, merge), and
//! reviews (with a protected-branch approval gate). Real handlers over
//! `rithub-core` (metadata) and `rithub-git` (git computation).
//!
//! Authorship is attributed from an optional `author`/`reviewer`/`merger`
//! username in the request body (resolved to a user id). Wiring these to the
//! authenticated Basic-auth identity is a later refinement; the plumbing is real
//! today.
//!
//! All side effects (webhook delivery, notification fan-out, audit appends,
//! email) are enqueued into `outbox_jobs` rather than executed inline. This
//! keeps slow or failing receivers off the hot request path and gives durable
//! retries with backoff via the `forge-jobs` worker.

use axum::extract::{Path, Query, State};
use axum::Json;
use rithub_core::{
    Issue, IssueState, IssueStore, MilestoneStore, NewIssue, NewPullRequest, NewReviewComment,
    NewWebhook, OutboxStore, PrState, PullRequest, PullRequestStore, Repository, Review,
    ReviewState, ReviewStore, StatusStore, Store, UpdateIssue, UpdatePr, Webhook, WebhookStore,
    KIND_AUDIT_APPEND, KIND_EMAIL_NOTIFICATION, KIND_NOTIFICATION_FAN_OUT, KIND_WEBHOOK_DELIVERY,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Outbox helpers: enqueue side effects without failing the request path.
// ---------------------------------------------------------------------------

/// Enqueue a webhook delivery job. Non-fatal: a failed enqueue is logged but
/// never propagates back to the caller.
fn enqueue_webhook(state: &AppState, repo_id: Uuid, event: &'static str, event_payload: serde_json::Value) {
    let state = state.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({
            "repo_id": repo_id,
            "event": event,
            "event_payload": event_payload,
        });
        state.store.enqueue_job(KIND_WEBHOOK_DELIVERY, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue webhook job"))
            .ok();
    });
}

/// Enqueue an in-app notification fan-out job.
fn enqueue_notify(
    state: &AppState,
    repo_id: Uuid,
    org_id: Uuid,
    actor_id: Option<Uuid>,
    kind: &'static str,
    title: String,
    link: Option<String>,
) {
    let state = state.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({
            "repo_id": repo_id,
            "org_id": org_id,
            "actor_id": actor_id,
            "kind": kind,
            "title": title,
            "link": link,
        });
        state.store.enqueue_job(KIND_NOTIFICATION_FAN_OUT, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue notification job"))
            .ok();
    });
}

/// Enqueue an email notification job (audience-scoped, same payload shape as
/// `notification_fan_out` but routed to SMTP instead of in-app).
fn enqueue_email(
    state: &AppState,
    repo_id: Uuid,
    org_id: Uuid,
    actor_id: Option<Uuid>,
    subject: String,
    body_text: String,
) {
    let state = state.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({
            "repo_id": repo_id,
            "org_id": org_id,
            "actor_id": actor_id,
            "subject": subject,
            "body_text": body_text,
        });
        state.store.enqueue_job(KIND_EMAIL_NOTIFICATION, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue email job"))
            .ok();
    });
}

/// Enqueue an audit append job.
fn enqueue_audit(
    state: &AppState,
    repo_id: Option<Uuid>,
    actor_id: Option<Uuid>,
    actor_name: &str,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    let state = state.clone();
    let actor_name = actor_name.to_string();
    let action = action.to_string();
    let target = target.map(|s| s.to_string());
    let detail = detail.map(|s| s.to_string());
    tokio::spawn(async move {
        let payload = serde_json::json!({
            "repo_id": repo_id,
            "actor_id": actor_id,
            "actor_name": actor_name,
            "action": action,
            "target": target,
            "detail": detail,
        });
        state.store.enqueue_job(KIND_AUDIT_APPEND, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue audit job"))
            .ok();
    });
}

// --- shared helpers ---------------------------------------------------------

/// Load repository metadata by (org, repo) or 404.
async fn repo_meta(state: &AppState, org: &str, repo: &str) -> Result<Repository, AppError> {
    Ok(state.store.get_repo(org, repo).await?)
}

/// Resolve an optional username to a user id; a provided-but-unknown user is a
/// 400 rather than silently dropped.
async fn resolve_user(
    state: &AppState,
    username: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    match username {
        None => Ok(None),
        Some(name) => Ok(Some(state.store.get_user(name).await?.id)),
    }
}

#[derive(Debug, Deserialize)]
pub struct StateFilter {
    #[serde(default)]
    pub state: Option<String>,
}

/// Resolve a PR's head commit: for a same-repo PR, the live head-branch tip; for
/// a cross-repo (fork) PR, the pinned `head_sha` already imported into this repo.
fn resolve_pr_head(
    state: &AppState,
    org: &str,
    repo: &str,
    pr: &PullRequest,
) -> Result<rithub_git::ObjectId, AppError> {
    if pr.head_repo_id.is_some() {
        let sha = pr
            .head_sha
            .as_deref()
            .ok_or_else(|| AppError::bad_request("cross-repo PR missing head sha"))?;
        rithub_git::ObjectId::from_hex(sha)
            .map_err(|_| AppError::bad_request("invalid head sha"))
    } else {
        state
            .host
            .resolve_branch(org, repo, &pr.head_branch)
            .map_err(AppError::from)
    }
}

// --- issues -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateIssue {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

pub async fn create_issue(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<CreateIssue>,
) -> Result<Json<Issue>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let author = resolve_user(&state, input.author.as_deref()).await?;
    let issue = state
        .store
        .create_issue(
            meta.id,
            author,
            &NewIssue {
                title: input.title,
                body: input.body,
            },
        )
        .await?;
    enqueue_webhook(
        &state,
        meta.id,
        "issue.opened",
        serde_json::json!({
            "event": "issue.opened",
            "repo": format!("{org}/{repo}"),
            "issue": { "number": issue.number, "title": issue.title },
        }),
    );
    enqueue_notify(
        &state,
        meta.id,
        meta.org_id,
        author,
        "issue.opened",
        format!("Issue #{} opened: {}", issue.number, issue.title),
        Some(format!("/{org}/{repo}/issues/{}", issue.number)),
    );
    enqueue_email(
        &state,
        meta.id,
        meta.org_id,
        author,
        format!("[{org}/{repo}] Issue #{} opened: {}", issue.number, issue.title),
        format!("A new issue was opened in {org}/{repo}.\n\n#{}: {}\n", issue.number, issue.title),
    );
    Ok(Json(issue))
}

pub async fn list_issues(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(filter): Query<StateFilter>,
) -> Result<Json<Vec<Issue>>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let state_filter = filter.state.as_deref().map(IssueState::parse);
    Ok(Json(state.store.list_issues(meta.id, state_filter).await?))
}

pub async fn get_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<IssueDetail>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let labels = state.store.list_issue_labels(issue.id).await?;
    let assignees = state
        .store
        .list_issue_assignees(issue.id)
        .await?
        .into_iter()
        .map(|u| u.username)
        .collect();
    let comments = state.store.list_issue_comments(issue.id).await?.len();
    Ok(Json(IssueDetail {
        issue,
        labels: labels.into_iter().map(|l| l.name).collect(),
        assignees,
        comment_count: comments,
    }))
}

#[derive(Debug, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub comment_count: usize,
}

pub async fn close_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Issue>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    Ok(Json(
        state
            .store
            .set_issue_state(meta.id, number, IssueState::Closed)
            .await?,
    ))
}

pub async fn reopen_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Issue>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    Ok(Json(
        state
            .store
            .set_issue_state(meta.id, number, IssueState::Open)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct CommentBody {
    pub body: String,
    #[serde(default)]
    pub author: Option<String>,
}

pub async fn comment_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<CommentBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let author = resolve_user(&state, input.author.as_deref()).await?;
    let comment = state
        .store
        .add_issue_comment(issue.id, author, &input.body)
        .await?;
    Ok(Json(serde_json::to_value(comment).unwrap()))
}

pub async fn list_issue_comments(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let comments = state.store.list_issue_comments(issue.id).await?;
    Ok(Json(serde_json::to_value(comments).unwrap()))
}

#[derive(Debug, Deserialize)]
pub struct LabelInput {
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
}

pub async fn label_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<LabelInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    // create_label upserts, so this ensures-then-attaches the label by name.
    let color = input.color.as_deref().unwrap_or("cccccc");
    let label = state.store.create_label(meta.id, &input.label, color).await?;
    state.store.add_issue_label(issue.id, label.id).await?;
    Ok(Json(serde_json::json!({ "label": label.name })))
}

#[derive(Debug, Deserialize)]
pub struct AssigneeInput {
    pub username: String,
}

pub async fn assign_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<AssigneeInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let issue = state.store.get_issue(meta.id, number).await?;
    let user = state.store.get_user(&input.username).await?;
    state.store.add_issue_assignee(issue.id, user.id).await?;
    Ok(Json(serde_json::json!({ "assignee": user.username })))
}

// --- PATCH /issues/{number} — update issue ----------------------------------

/// Update request for an issue. All fields are optional.
#[derive(Debug, Deserialize)]
pub struct UpdateIssueInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// State transition: `"open"` or `"closed"`.
    #[serde(default)]
    pub state: Option<String>,
    /// Milestone number to assign, or `0` to clear the milestone.
    #[serde(default)]
    pub milestone: Option<i32>,
    /// Replaces the full assignee list when provided.
    #[serde(default)]
    pub assignees: Option<Vec<String>>,
    /// Replaces the full label list when provided. Each entry is `{ label, color? }`.
    #[serde(default)]
    pub labels: Option<Vec<LabelInput>>,
}

pub async fn update_issue(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<UpdateIssueInput>,
) -> Result<Json<IssueDetail>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;

    // Resolve optional milestone number → id.
    let (set_milestone, milestone_id) = match input.milestone {
        None => (false, None),
        Some(0) => (true, None), // explicit clear
        Some(n) => {
            let mid = state.store.get_milestone_id(meta.id, n).await?;
            (true, Some(mid))
        }
    };

    // Update core fields (title, body, milestone).
    state
        .store
        .update_issue(
            meta.id,
            number,
            &UpdateIssue {
                title: input.title,
                body: input.body,
                set_milestone,
                milestone_id,
            },
        )
        .await?;

    // State transition (close / reopen) — uses the existing stamped path.
    if let Some(ref s) = input.state {
        let target = IssueState::parse(s);
        state.store.set_issue_state(meta.id, number, target).await?;
    }

    // Replace assignees if the caller provided a list.
    if let Some(ref usernames) = input.assignees {
        let mut ids = Vec::with_capacity(usernames.len());
        for name in usernames {
            ids.push(state.store.get_user(name).await?.id);
        }
        let issue = state.store.get_issue(meta.id, number).await?;
        state.store.replace_issue_assignees(issue.id, &ids).await?;
    }

    // Replace labels if the caller provided a list.
    if let Some(ref label_inputs) = input.labels {
        let issue = state.store.get_issue(meta.id, number).await?;
        let mut label_ids = Vec::with_capacity(label_inputs.len());
        for li in label_inputs {
            let color = li.color.as_deref().unwrap_or("cccccc");
            let label = state.store.create_label(meta.id, &li.label, color).await?;
            label_ids.push(label.id);
        }
        state
            .store
            .replace_issue_labels(issue.id, &label_ids)
            .await?;
    }

    // Re-fetch the canonical view.
    let issue = state.store.get_issue(meta.id, number).await?;
    let labels = state.store.list_issue_labels(issue.id).await?;
    let assignees = state
        .store
        .list_issue_assignees(issue.id)
        .await?
        .into_iter()
        .map(|u| u.username)
        .collect();
    let comment_count = state.store.list_issue_comments(issue.id).await?.len();
    Ok(Json(IssueDetail {
        issue,
        labels: labels.into_iter().map(|l| l.name).collect(),
        assignees,
        comment_count,
    }))
}

// --- pull requests ----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePr {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub head: String,
    pub base: String,
    #[serde(default)]
    pub author: Option<String>,
    /// For a **cross-repo** PR: the source `org/repo` the head branch lives in.
    #[serde(default)]
    pub head_repo: Option<String>,
}

pub async fn create_pr(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<CreatePr>,
) -> Result<Json<PullRequest>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let base_oid = state
        .host
        .resolve_branch(&org, &repo, &input.base)
        .map_err(|_| AppError::bad_request(format!("base branch not found: {}", input.base)))?;

    // Resolve the head: same-repo (a branch here) or cross-repo (a fork's branch).
    let self_slug = format!("{org}/{repo}");
    let cross = input
        .head_repo
        .as_deref()
        .filter(|hr| *hr != self_slug)
        .map(|hr| hr.to_string());
    let (head_repo_id, head_oid, source): (Option<uuid::Uuid>, rithub_git::ObjectId, Option<(String, String)>) =
        match &cross {
            Some(head_repo) => {
                let (ho, hn) = head_repo
                    .split_once('/')
                    .ok_or_else(|| AppError::bad_request("head_repo must be org/repo"))?;
                let hmeta = state.store.get_repo(ho, hn).await?;
                let hoid = state
                    .host
                    .resolve_branch(ho, hn, &input.head)
                    .map_err(|_| AppError::bad_request(format!("head branch not found: {}", input.head)))?;
                (Some(hmeta.id), hoid, Some((ho.to_string(), hn.to_string())))
            }
            None => {
                if input.head == input.base {
                    return Err(AppError::bad_request("head and base are the same branch"));
                }
                let hoid = state
                    .host
                    .resolve_branch(&org, &repo, &input.head)
                    .map_err(|_| AppError::bad_request(format!("head branch not found: {}", input.head)))?;
                (None, hoid, None)
            }
        };

    let author = resolve_user(&state, input.author.as_deref()).await?;
    let pr = state
        .store
        .create_pr(
            meta.id,
            author,
            &NewPullRequest {
                title: input.title,
                body: input.body,
                head: input.head,
                base: input.base,
            },
            Some(&base_oid.to_hex()),
            Some(&head_oid.to_hex()),
            head_repo_id,
        )
        .await?;

    // For a cross-repo PR, import the fork head's objects into this repo and
    // pin refs/pull/<n>/head so diff/merge resolve entirely locally.
    if let Some((ho, hn)) = source {
        state
            .host
            .import_commit(&ho, &hn, &org, &repo, head_oid, &format!("refs/pull/{}/head", pr.number))
            .map_err(AppError::from)?;
    }
    enqueue_webhook(
        &state,
        meta.id,
        "pr.opened",
        serde_json::json!({
            "event": "pr.opened",
            "repo": format!("{org}/{repo}"),
            "pull_request": { "number": pr.number, "title": pr.title,
                              "head": pr.head_branch, "base": pr.base_branch },
        }),
    );
    enqueue_notify(
        &state,
        meta.id,
        meta.org_id,
        author,
        "pr.opened",
        format!("PR #{} opened: {}", pr.number, pr.title),
        Some(format!("/{org}/{repo}/pulls/{}", pr.number)),
    );
    enqueue_email(
        &state,
        meta.id,
        meta.org_id,
        author,
        format!("[{org}/{repo}] PR #{} opened: {}", pr.number, pr.title),
        format!(
            "A new pull request was opened in {org}/{repo}.\n\nPR #{}: {}\n{head} → {base}\n",
            pr.number,
            pr.title,
            head = pr.head_branch,
            base = pr.base_branch,
        ),
    );
    Ok(Json(pr))
}

pub async fn list_prs(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(filter): Query<StateFilter>,
) -> Result<Json<Vec<PullRequest>>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let state_filter = filter.state.as_deref().map(PrState::parse);
    Ok(Json(state.store.list_prs(meta.id, state_filter).await?))
}

pub async fn get_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<PrDetail>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let analysis = current_mergeability(&state, &org, &repo, &pr).await.ok();
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

#[derive(Debug, Serialize)]
pub struct PrDetail {
    #[serde(flatten)]
    pub pr: PullRequest,
    pub mergeable: Option<bool>,
    pub merge_status: Option<String>,
    pub approvals: i64,
}

/// Resolve the PR's live base/head tips and analyze mergeability.
struct LiveMergeability {
    status: String,
    mergeable: bool,
    base_sha: String,
    head_sha: String,
    merge_base: Option<String>,
}

async fn current_mergeability(
    state: &AppState,
    org: &str,
    repo: &str,
    pr: &PullRequest,
) -> Result<LiveMergeability, AppError> {
    let base = state
        .host
        .resolve_branch(org, repo, &pr.base_branch)
        .map_err(AppError::from)?;
    let head = resolve_pr_head(state, org, repo, pr)?;
    let analysis = state
        .host
        .mergeability(org, repo, base, head)
        .map_err(AppError::from)?;
    Ok(LiveMergeability {
        status: analysis.status.as_str().to_string(),
        mergeable: analysis.status.mergeable(),
        base_sha: analysis.base_sha,
        head_sha: analysis.head_sha,
        merge_base: analysis.merge_base,
    })
}

pub async fn pr_mergeability(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let m = current_mergeability(&state, &org, &repo, &pr).await?;
    Ok(Json(serde_json::json!({
        "status": m.status,
        "mergeable": m.mergeable,
        "base_sha": m.base_sha,
        "head_sha": m.head_sha,
        "merge_base": m.merge_base,
    })))
}

pub async fn pr_diff(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<DiffDto>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let base = state
        .host
        .resolve_branch(&org, &repo, &pr.base_branch)
        .map_err(AppError::from)?;
    let head = resolve_pr_head(&state, &org, &repo, &pr)?;
    let diff = state.host.pr_diff(&org, &repo, base, head).map_err(AppError::from)?;
    Ok(Json(DiffDto::from(diff)))
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

pub async fn pr_commits(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let base = state.host.resolve_branch(&org, &repo, &pr.base_branch).map_err(AppError::from)?;
    let head = resolve_pr_head(&state, &org, &repo, &pr)?;
    let commits = state.host.commit_list(&org, &repo, base, head).map_err(AppError::from)?;
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
    Json(input): Json<CommentBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let author = resolve_user(&state, input.author.as_deref()).await?;
    let comment = state.store.add_pr_comment(pr.id, author, &input.body).await?;
    Ok(Json(serde_json::to_value(comment).unwrap()))
}

pub async fn close_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<PullRequest>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    Ok(Json(state.store.close_pr(meta.id, number).await?))
}

// --- PATCH /pulls/{number} — update PR -------------------------------------

/// Update request for a pull request. All fields optional.
#[derive(Debug, Deserialize)]
pub struct UpdatePrInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// New base branch (changes the merge target; only meaningful while open).
    #[serde(default)]
    pub base: Option<String>,
    /// State transition: `"open"` (reopen closed) or `"closed"` (close open).
    #[serde(default)]
    pub state: Option<String>,
    /// Milestone number to assign, or `0` to clear.
    #[serde(default)]
    pub milestone: Option<i32>,
}

pub async fn update_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<UpdatePrInput>,
) -> Result<Json<PullRequest>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;

    // Resolve optional milestone number → id.
    let (set_milestone, milestone_id) = match input.milestone {
        None => (false, None),
        Some(0) => (true, None),
        Some(n) => {
            let mid = state.store.get_milestone_id(meta.id, n).await?;
            (true, Some(mid))
        }
    };

    // Update metadata fields.
    state
        .store
        .update_pr(
            meta.id,
            number,
            &UpdatePr {
                title: input.title,
                body: input.body,
                base: input.base,
                state: None, // handled below
                set_milestone,
                milestone_id,
            },
        )
        .await?;

    // State transition — cannot change a merged PR; reopen only works on closed.
    let pr = match input.state.as_deref() {
        Some("closed") => state.store.close_pr(meta.id, number).await?,
        Some("open") => state.store.reopen_pr(meta.id, number).await?,
        _ => state.store.get_pr(meta.id, number).await?,
    };

    Ok(Json(pr))
}

#[derive(Debug, Deserialize)]
pub struct MergeInput {
    #[serde(default)]
    pub merger: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// `"merge"` (default), `"squash"`, or `"rebase"`.
    #[serde(default)]
    pub strategy: Option<String>,
}

pub async fn merge_pr(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<MergeInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    if pr.state != PrState::Open {
        return Err(AppError::conflict("pull request is not open"));
    }

    // Pin the PR head to a single oid up front, so the approval/status gate and
    // the merge itself act on the *same* commit. Resolving twice would open a
    // TOCTOU window: a push landing between the gate and the merge could sneak
    // an unreviewed / unchecked commit past the gate.
    let head_oid = resolve_pr_head(&state, &org, &repo, &pr)?;

    // Protected-branch gate: required approvals + required status checks.
    if let Some(protection) = state
        .store
        .get_branch_protection(meta.id, &pr.base_branch)
        .await?
    {
        let approvals = state
            .store
            .count_current_approvals(pr.id, pr.author_id)
            .await?;
        if approvals < protection.required_approvals as i64 {
            return Err(AppError::forbidden(format!(
                "branch '{}' requires {} approval(s); have {}",
                pr.base_branch, protection.required_approvals, approvals
            )));
        }
        if !protection.required_checks.is_empty() {
            let pass = state
                .store
                .required_checks_pass(meta.id, &head_oid.to_hex(), &protection.required_checks)
                .await?;
            if !pass {
                return Err(AppError::forbidden(format!(
                    "branch '{}' requires status checks to pass: {}",
                    pr.base_branch,
                    protection.required_checks.join(", ")
                )));
            }
        }
    }

    let merger = resolve_user(&state, input.merger.as_deref()).await?;
    let strategy = rithub_git::MergeStrategy::parse(input.strategy.as_deref().unwrap_or("merge"));
    let message = input
        .message
        .unwrap_or_else(|| format!("Merge pull request #{number}: {}", pr.title));

    let result = state
        .host
        .merge(
            &org,
            &repo,
            &pr.base_branch,
            head_oid,
            strategy,
            input.merger.as_deref().unwrap_or("rithub"),
            "rithub@localhost",
            &message,
        )
        .map_err(|e| match e {
            rithub_git::GitHostError::Invalid(m) => AppError::conflict(m),
            other => AppError::from(other),
        })?;

    let merged = state
        .store
        .mark_merged(meta.id, number, &result.merge_commit_sha, merger)
        .await?;

    let merger_name = input.merger.as_deref().unwrap_or("rithub").to_string();
    enqueue_audit(
        &state,
        Some(meta.id),
        merger,
        &merger_name,
        "pr.merged",
        Some(&format!("#{number}")),
        Some(&format!("{} into {}", pr.head_branch, pr.base_branch)),
    );
    enqueue_notify(
        &state,
        meta.id,
        meta.org_id,
        merger,
        "pr.merged",
        format!("PR #{number} merged: {}", pr.title),
        Some(format!("/{org}/{repo}/pulls/{number}")),
    );
    enqueue_email(
        &state,
        meta.id,
        meta.org_id,
        merger,
        format!("[{org}/{repo}] PR #{number} merged: {}", pr.title),
        format!(
            "Pull request #{number} was merged into {base} in {org}/{repo}.\n\nMerge commit: {sha}\n",
            base = pr.base_branch,
            sha = result.merge_commit_sha,
        ),
    );
    enqueue_webhook(
        &state,
        meta.id,
        "pr.merged",
        serde_json::json!({
            "event": "pr.merged",
            "repo": format!("{org}/{repo}"),
            "pull_request": { "number": number, "title": pr.title },
            "merge_commit_sha": result.merge_commit_sha,
            "kind": result.kind,
        }),
    );

    Ok(Json(serde_json::json!({
        "merged": true,
        "kind": result.kind,
        "merge_commit_sha": result.merge_commit_sha,
        "new_base_sha": result.new_base_sha,
        "pull_request": merged,
    })))
}

// --- reviews ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateReview {
    pub state: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub comments: Vec<NewReviewComment>,
}

pub async fn create_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<CreateReview>,
) -> Result<Json<Review>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let review_state = ReviewState::parse(&input.state)
        .ok_or_else(|| AppError::bad_request("state must be approved|changes_requested|commented"))?;
    let reviewer = resolve_user(&state, input.reviewer.as_deref()).await?;
    let review = state
        .store
        .create_review(
            pr.id,
            reviewer,
            review_state,
            input.body.as_deref(),
            &input.comments,
        )
        .await?;
    // Fan out in-app notification + email for the review event.
    let review_title = format!("Review ({}) on PR #{number}", review_state.as_str());
    enqueue_notify(
        &state,
        meta.id,
        meta.org_id,
        reviewer,
        "review",
        review_title.clone(),
        Some(format!("/{org}/{repo}/pulls/{number}")),
    );
    enqueue_email(
        &state,
        meta.id,
        meta.org_id,
        reviewer,
        format!("[{org}/{repo}] {review_title}"),
        format!(
            "A review was submitted on PR #{number} in {org}/{repo}.\n\nState: {}\n",
            review_state.as_str(),
        ),
    );
    Ok(Json(review))
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
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

pub async fn set_protection(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
    Json(input): Json<ProtectionInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let protection = state
        .store
        .set_branch_protection(
            meta.id,
            &branch,
            input.required_approvals,
            input.block_direct_push,
            &input.required_checks,
        )
        .await?;
    enqueue_audit(
        &state,
        Some(meta.id),
        None,
        "admin",
        "protection.changed",
        Some(&branch),
        Some(&format!(
            "approvals={}, block_push={}, checks=[{}]",
            input.required_approvals,
            input.block_direct_push,
            input.required_checks.join(",")
        )),
    );
    Ok(Json(serde_json::to_value(protection).unwrap()))
}

pub async fn get_protection(
    State(state): State<AppState>,
    Path((org, repo, branch)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    let protection = state.store.get_branch_protection(meta.id, &branch).await?;
    Ok(Json(serde_json::json!({ "protection": protection })))
}

// --- webhooks ---------------------------------------------------------------

pub async fn create_hook(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<NewWebhook>,
) -> Result<Json<Webhook>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    Ok(Json(state.store.create_webhook(meta.id, &input).await?))
}

pub async fn list_hooks(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Webhook>>, AppError> {
    let meta = repo_meta(&state, &org, &repo).await?;
    Ok(Json(state.store.list_webhooks(meta.id).await?))
}

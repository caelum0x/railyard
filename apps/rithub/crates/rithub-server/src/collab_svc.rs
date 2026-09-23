//! Collaboration **service layer**: the single implementation of every issue,
//! pull-request, and review mutation, shared by the JSON API
//! ([`crate::collab_api`]) and the web UI ([`crate::web`]).
//!
//! Callers authorize first (see [`crate::authz`]) and pass the acting [`User`];
//! the service performs the write and then fans out every side effect — webhook
//! delivery, in-app notification, email, audit, CI dispatch, and search
//! indexing — through the durable `outbox_jobs` queue, so an issue opened in
//! the browser behaves exactly like one opened over the API.

use rithub_core::{
    DirectoryStore, Issue, IssueComment, IssueState, IssueStore, MilestoneStore, NewIssue, OutboxStore,
    PullRequest, Repository, Store, UpdateIssue, User, KIND_AUDIT_APPEND, KIND_EMAIL_NOTIFICATION,
    KIND_NOTIFICATION_FAN_OUT, KIND_WEBHOOK_DELIVERY,
};
use serde::Serialize;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

pub use crate::pulls_svc::{
    comment_pr, create_pr, create_review, merge_pr, set_pr_state, update_pr, MergeOutcome,
    PrInput, PrPatch,
};

/// The repository a mutation targets: its `org/name` path plus metadata.
pub struct RepoRef<'a> {
    pub org: &'a str,
    pub repo: &'a str,
    pub meta: &'a Repository,
}

impl RepoRef<'_> {
    pub(crate) fn slug(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }
}

// ---------------------------------------------------------------------------
// Outbox helpers: enqueue side effects without failing the request path.
// ---------------------------------------------------------------------------

fn enqueue(state: &AppState, kind: &'static str, payload: serde_json::Value) {
    let state = state.clone();
    tokio::spawn(async move {
        state
            .store
            .enqueue_job(kind, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, kind, "failed to enqueue outbox job"))
            .ok();
    });
}

/// Enqueue a signed webhook delivery for `event`.
pub fn enqueue_webhook(state: &AppState, repo_id: Uuid, event: &'static str, body: serde_json::Value) {
    enqueue(
        state,
        KIND_WEBHOOK_DELIVERY,
        serde_json::json!({ "repo_id": repo_id, "event": event, "event_payload": body }),
    );
}

/// Enqueue an in-app notification fan-out to the repo audience.
pub fn enqueue_notify(
    state: &AppState,
    r: &RepoRef<'_>,
    actor_id: Option<Uuid>,
    kind: &'static str,
    title: String,
    link: String,
) {
    enqueue(
        state,
        KIND_NOTIFICATION_FAN_OUT,
        serde_json::json!({
            "repo_id": r.meta.id, "org_id": r.meta.org_id, "actor_id": actor_id,
            "kind": kind, "title": title, "link": link,
        }),
    );
}

/// Enqueue an email notification to the repo audience.
pub fn enqueue_email(
    state: &AppState,
    r: &RepoRef<'_>,
    actor_id: Option<Uuid>,
    subject: String,
    body_text: String,
) {
    enqueue(
        state,
        KIND_EMAIL_NOTIFICATION,
        serde_json::json!({
            "repo_id": r.meta.id, "org_id": r.meta.org_id, "actor_id": actor_id,
            "subject": subject, "body_text": body_text,
        }),
    );
}

/// Enqueue an audit-log append.
pub fn enqueue_audit(
    state: &AppState,
    repo_id: Option<Uuid>,
    actor: Option<&User>,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    enqueue(
        state,
        KIND_AUDIT_APPEND,
        serde_json::json!({
            "repo_id": repo_id,
            "actor_id": actor.map(|u| u.id),
            "actor_name": actor.map(|u| u.username.as_str()).unwrap_or("system"),
            "action": action, "target": target, "detail": detail,
        }),
    );
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Whether `user` authored the item or holds ≥`needed` on the repo.
pub async fn is_author_or_role(
    state: &AppState,
    user: &User,
    meta: &Repository,
    author_id: Option<Uuid>,
    needed: rithub_core::RepoRole,
) -> bool {
    if author_id == Some(user.id) {
        return true;
    }
    matches!(crate::authz::user_repo_role(state, user, meta).await, Some(r) if r >= needed)
}

/// Resolve a PR's head commit: for a same-repo PR, the live head-branch tip; for
/// a cross-repo (fork) PR, the pinned `head_sha` already imported into this repo.
pub fn resolve_pr_head(
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
        rithub_git::ObjectId::from_hex(sha).map_err(|_| AppError::bad_request("invalid head sha"))
    } else {
        state
            .host
            .resolve_branch(org, repo, &pr.head_branch)
            .map_err(AppError::from)
    }
}

/// Live mergeability of a PR (base tip vs. head).
pub struct LiveMergeability {
    pub status: String,
    pub mergeable: bool,
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: Option<String>,
}

pub fn current_mergeability(
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

/// The PR diff (base tip vs. head).
pub fn pr_diff(
    state: &AppState,
    org: &str,
    repo: &str,
    pr: &PullRequest,
) -> Result<rithub_git::DiffResult, AppError> {
    let base = state
        .host
        .resolve_branch(org, repo, &pr.base_branch)
        .map_err(AppError::from)?;
    let head = resolve_pr_head(state, org, repo, pr)?;
    state.host.pr_diff(org, repo, base, head).map_err(AppError::from)
}

pub(crate) fn validate_title(title: &str) -> Result<String, AppError> {
    let t = title.trim();
    if t.is_empty() {
        return Err(AppError::bad_request("title is required"));
    }
    if t.chars().count() > 256 {
        return Err(AppError::bad_request("title must be at most 256 characters"));
    }
    Ok(t.to_string())
}

pub(crate) fn non_empty(body: Option<String>) -> Option<String> {
    body.filter(|b| !b.trim().is_empty())
}

async fn index_issue(state: &AppState, r: &RepoRef<'_>, issue: &Issue) {
    crate::search_index::index_issue(
        state,
        r.org,
        r.meta,
        issue.number as i64,
        &issue.title,
        issue.body.as_deref().unwrap_or(""),
    )
    .await;
}

pub(crate) async fn index_pull(state: &AppState, r: &RepoRef<'_>, pr: &PullRequest) {
    crate::search_index::index_pull(
        state,
        r.org,
        r.meta,
        pr.number as i64,
        &pr.title,
        pr.body.as_deref().unwrap_or(""),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// Open an issue.
pub async fn create_issue(
    state: &AppState,
    r: &RepoRef<'_>,
    author: &User,
    title: &str,
    body: Option<String>,
) -> Result<Issue, AppError> {
    let title = validate_title(title)?;
    let issue = state
        .store
        .create_issue(r.meta.id, Some(author.id), &NewIssue { title, body: non_empty(body) })
        .await?;
    let link = format!("/{}/issues/{}", r.slug(), issue.number);
    enqueue_webhook(
        state,
        r.meta.id,
        "issue.opened",
        serde_json::json!({
            "event": "issue.opened", "repo": r.slug(), "sender": author.username,
            "issue": { "number": issue.number, "title": issue.title },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(author.id),
        "issue.opened",
        format!("Issue #{} opened: {}", issue.number, issue.title),
        link,
    );
    enqueue_email(
        state,
        r,
        Some(author.id),
        format!("[{}] Issue #{} opened: {}", r.slug(), issue.number, issue.title),
        format!(
            "{} opened an issue in {}.\n\n#{}: {}\n",
            author.username,
            r.slug(),
            issue.number,
            issue.title
        ),
    );
    index_issue(state, r, &issue).await;
    Ok(issue)
}

/// Comment on an issue.
pub async fn comment_issue(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    author: &User,
    body: &str,
) -> Result<IssueComment, AppError> {
    if body.trim().is_empty() {
        return Err(AppError::bad_request("comment body is required"));
    }
    let issue = state.store.get_issue(r.meta.id, number).await?;
    let comment = state
        .store
        .add_issue_comment(issue.id, Some(author.id), body)
        .await?;
    enqueue_webhook(
        state,
        r.meta.id,
        "issue.commented",
        serde_json::json!({
            "event": "issue.commented", "repo": r.slug(), "sender": author.username,
            "issue": { "number": issue.number, "title": issue.title },
            "comment": { "id": comment.id, "body": comment.body },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(author.id),
        "issue.commented",
        format!("{} commented on #{}: {}", author.username, issue.number, issue.title),
        format!("/{}/issues/{}", r.slug(), issue.number),
    );
    Ok(comment)
}

/// Close or reopen an issue.
pub async fn set_issue_state(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    actor: &User,
    target: IssueState,
) -> Result<Issue, AppError> {
    let issue = state.store.set_issue_state(r.meta.id, number, target).await?;
    let event: &'static str = if target == IssueState::Closed {
        "issue.closed"
    } else {
        "issue.reopened"
    };
    enqueue_webhook(
        state,
        r.meta.id,
        event,
        serde_json::json!({
            "event": event, "repo": r.slug(), "sender": actor.username,
            "issue": { "number": issue.number, "title": issue.title },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(actor.id),
        event,
        format!(
            "Issue #{} {} by {}: {}",
            issue.number,
            target.as_str(),
            actor.username,
            issue.title
        ),
        format!("/{}/issues/{}", r.slug(), issue.number),
    );
    index_issue(state, r, &issue).await;
    Ok(issue)
}

/// The full, canonical view of an issue (labels, assignees, comment count).
#[derive(Debug, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub comment_count: usize,
}

pub async fn issue_detail(state: &AppState, meta: &Repository, number: i32) -> Result<IssueDetail, AppError> {
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
    Ok(IssueDetail {
        issue,
        labels: labels.into_iter().map(|l| l.name).collect(),
        assignees,
        comment_count,
    })
}

/// A label to set on an issue (created on first use).
pub struct LabelSpec {
    pub name: String,
    pub color: Option<String>,
}

/// Validate a label color as 6 hex digits (with or without `#`).
pub(crate) fn normalize_color(color: Option<&str>) -> Result<String, AppError> {
    let c = color.unwrap_or("cccccc").trim().trim_start_matches('#');
    if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(c.to_ascii_lowercase())
    } else {
        Err(AppError::bad_request("label color must be 6 hex digits"))
    }
}

/// A partial issue update. Callers must have authorized the fields they pass:
/// title/body/state need author-or-Write; labels/assignees/milestone need Write.
#[derive(Default)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<IssueState>,
    /// `Some(0)` clears the milestone.
    pub milestone: Option<i32>,
    pub assignees: Option<Vec<String>>,
    pub labels: Option<Vec<LabelSpec>>,
}

/// Apply an issue patch, then fan out events and re-index.
pub async fn update_issue(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    actor: &User,
    patch: IssuePatch,
) -> Result<IssueDetail, AppError> {
    let (set_milestone, milestone_id) = match patch.milestone {
        None => (false, None),
        Some(0) => (true, None),
        Some(n) => (true, Some(state.store.get_milestone_id(r.meta.id, n).await?)),
    };
    let title = patch.title.as_deref().map(validate_title).transpose()?;
    state
        .store
        .update_issue(
            r.meta.id,
            number,
            &UpdateIssue {
                title,
                body: patch.body,
                set_milestone,
                milestone_id,
            },
        )
        .await?;
    let issue = state.store.get_issue(r.meta.id, number).await?;

    if let Some(usernames) = &patch.assignees {
        let mut ids = Vec::with_capacity(usernames.len());
        for name in usernames.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            let user = state.store.get_user(name).await?;
            if !crate::authz::user_can_read(state, Some(&user), r.meta).await {
                return Err(AppError::bad_request(format!(
                    "{name} cannot be assigned: no access to this repository"
                )));
            }
            ids.push(user.id);
        }
        state.store.replace_issue_assignees(issue.id, &ids).await?;
    }
    if let Some(labels) = &patch.labels {
        let mut ids = Vec::with_capacity(labels.len());
        for spec in labels.iter().filter(|l| !l.name.trim().is_empty()) {
            let color = normalize_color(spec.color.as_deref())?;
            let label = state
                .store
                .create_label(r.meta.id, spec.name.trim(), &color)
                .await?;
            ids.push(label.id);
        }
        state.store.replace_issue_labels(issue.id, &ids).await?;
    }
    if let Some(target) = patch.state {
        if target != issue.state {
            set_issue_state(state, r, number, actor, target).await?;
        }
    }

    let detail = issue_detail(state, r.meta, number).await?;
    enqueue_webhook(
        state,
        r.meta.id,
        "issue.updated",
        serde_json::json!({
            "event": "issue.updated", "repo": r.slug(), "sender": actor.username,
            "issue": { "number": detail.issue.number, "title": detail.issue.title,
                       "labels": detail.labels, "assignees": detail.assignees },
        }),
    );
    index_issue(state, r, &detail.issue).await;
    Ok(detail)
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

fn validate_label_name(name: &str) -> Result<&str, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 50 {
        return Err(AppError::bad_request("label name must be 1-50 characters"));
    }
    Ok(name)
}

/// Create (or recolor, if it exists) a repository label.
pub async fn create_label(
    state: &AppState,
    meta: &Repository,
    actor: &User,
    name: &str,
    color: Option<&str>,
) -> Result<rithub_core::Label, AppError> {
    let name = validate_label_name(name)?;
    let color = normalize_color(color)?;
    let label = state.store.create_label(meta.id, name, &color).await?;
    enqueue_audit(state, Some(meta.id), Some(actor), "label.created", Some(&label.name), Some(&color));
    Ok(label)
}

/// Rename and/or recolor the label `label_id` (absent fields are unchanged).
pub async fn edit_label(
    state: &AppState,
    meta: &Repository,
    actor: &User,
    label_id: Uuid,
    name: Option<&str>,
    color: Option<&str>,
) -> Result<rithub_core::Label, AppError> {
    let existing = state
        .store
        .list_labels(meta.id)
        .await?
        .into_iter()
        .find(|l| l.id == label_id)
        .ok_or_else(|| AppError::not_found("label not found"))?;
    let name = validate_label_name(name.unwrap_or(&existing.name))?;
    let color = match color {
        Some(c) => normalize_color(Some(c))?,
        None => existing.color.clone(),
    };
    let label = state.store.update_label(meta.id, label_id, name, &color).await?;
    enqueue_audit(state, Some(meta.id), Some(actor), "label.updated", Some(&label.name), Some(&color));
    Ok(label)
}

/// Delete the label `label_id` (removed from every issue).
pub async fn delete_label(
    state: &AppState,
    meta: &Repository,
    actor: &User,
    label_id: Uuid,
) -> Result<(), AppError> {
    let labels = state.store.list_labels(meta.id).await?;
    let label = labels
        .into_iter()
        .find(|l| l.id == label_id)
        .ok_or_else(|| AppError::not_found("label not found"))?;
    state.store.delete_label(meta.id, label_id).await?;
    enqueue_audit(state, Some(meta.id), Some(actor), "label.deleted", Some(&label.name), None);
    Ok(())
}

/// Find a label by exact name.
pub async fn label_by_name(
    state: &AppState,
    meta: &Repository,
    name: &str,
) -> Result<rithub_core::Label, AppError> {
    state
        .store
        .list_labels(meta.id)
        .await?
        .into_iter()
        .find(|l| l.name == name)
        .ok_or_else(|| AppError::not_found(format!("label not found: {name}")))
}

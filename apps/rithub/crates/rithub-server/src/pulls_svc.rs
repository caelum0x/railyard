//! Pull-request half of the collaboration service layer (open, comment,
//! close/reopen, edit, review, merge). Re-exported through
//! [`crate::collab_svc`]; see that module for the shared contract.

use rithub_core::{
    NewPullRequest, NewReviewComment, PrComment, PrState, PullRequest, PullRequestStore, Review,
    ReviewState, ReviewStore, StatusStore, Store, UpdatePr, User, MilestoneStore,
};
use serde::Serialize;

use crate::collab_svc::{
    enqueue_audit, enqueue_email, enqueue_notify, enqueue_webhook, index_pull, non_empty,
    resolve_pr_head, validate_title, RepoRef,
};
use crate::error::AppError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------

/// Input for opening a pull request.
pub struct PrInput {
    pub title: String,
    pub body: Option<String>,
    pub head: String,
    pub base: String,
    /// Source `org/repo` for a cross-repo (fork) PR; `None`/self = same repo.
    pub head_repo: Option<String>,
}

/// Open a pull request (same-repo or from a fork the author can read).
pub async fn create_pr(
    state: &AppState,
    r: &RepoRef<'_>,
    author: &User,
    input: PrInput,
) -> Result<PullRequest, AppError> {
    let title = validate_title(&input.title)?;
    let base_oid = state
        .host
        .resolve_branch(r.org, r.repo, &input.base)
        .map_err(|_| AppError::bad_request(format!("base branch not found: {}", input.base)))?;

    let self_slug = r.slug();
    let cross = input
        .head_repo
        .as_deref()
        .map(str::trim)
        .filter(|hr| !hr.is_empty() && *hr != self_slug)
        .map(str::to_string);
    let (head_repo_id, head_oid, source) = match &cross {
        Some(head_repo) => {
            let (ho, hn) = head_repo
                .split_once('/')
                .ok_or_else(|| AppError::bad_request("head_repo must be org/repo"))?;
            let hmeta = state.store.get_repo(ho, hn).await?;
            if !crate::authz::user_can_read(state, Some(author), &hmeta).await {
                return Err(AppError::not_found(format!("repository not found: {head_repo}")));
            }
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
                .resolve_branch(r.org, r.repo, &input.head)
                .map_err(|_| AppError::bad_request(format!("head branch not found: {}", input.head)))?;
            (None, hoid, None)
        }
    };

    let pr = state
        .store
        .create_pr(
            r.meta.id,
            Some(author.id),
            &NewPullRequest {
                title,
                body: non_empty(input.body),
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
            .import_commit(&ho, &hn, r.org, r.repo, head_oid, &format!("refs/pull/{}/head", pr.number))
            .map_err(AppError::from)?;
    }
    let link = format!("/{}/pulls/{}", r.slug(), pr.number);
    enqueue_webhook(
        state,
        r.meta.id,
        "pr.opened",
        serde_json::json!({
            "event": "pr.opened", "repo": r.slug(), "sender": author.username,
            "pull_request": { "number": pr.number, "title": pr.title,
                              "head": pr.head_branch, "base": pr.base_branch,
                              "head_repo": cross },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(author.id),
        "pr.opened",
        format!("PR #{} opened: {}", pr.number, pr.title),
        link,
    );
    enqueue_email(
        state,
        r,
        Some(author.id),
        format!("[{}] PR #{} opened: {}", r.slug(), pr.number, pr.title),
        format!(
            "{} opened a pull request in {}.\n\nPR #{}: {}\n{} -> {}\n",
            author.username,
            r.slug(),
            pr.number,
            pr.title,
            pr.head_branch,
            pr.base_branch,
        ),
    );
    if let Some(head_sha) = pr.head_sha.as_deref() {
        crate::ci::dispatch_check_runs(
            state,
            r.meta.id,
            r.org,
            r.repo,
            head_sha,
            &pr.head_branch,
            "pull_request",
        );
    }
    index_pull(state, r, &pr).await;
    Ok(pr)
}

/// Comment on a pull request's conversation.
pub async fn comment_pr(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    author: &User,
    body: &str,
) -> Result<PrComment, AppError> {
    if body.trim().is_empty() {
        return Err(AppError::bad_request("comment body is required"));
    }
    let pr = state.store.get_pr(r.meta.id, number).await?;
    let comment = state.store.add_pr_comment(pr.id, Some(author.id), body).await?;
    enqueue_webhook(
        state,
        r.meta.id,
        "pr.commented",
        serde_json::json!({
            "event": "pr.commented", "repo": r.slug(), "sender": author.username,
            "pull_request": { "number": pr.number, "title": pr.title },
            "comment": { "id": comment.id, "body": comment.body },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(author.id),
        "pr.commented",
        format!("{} commented on PR #{}: {}", author.username, pr.number, pr.title),
        format!("/{}/pulls/{}", r.slug(), pr.number),
    );
    Ok(comment)
}

/// Close (`open = false`) or reopen (`open = true`) a pull request.
pub async fn set_pr_state(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    actor: &User,
    open: bool,
) -> Result<PullRequest, AppError> {
    let current = state.store.get_pr(r.meta.id, number).await?;
    if current.state == PrState::Merged {
        return Err(AppError::conflict("a merged pull request cannot change state"));
    }
    let pr = if open {
        state.store.reopen_pr(r.meta.id, number).await?
    } else {
        state.store.close_pr(r.meta.id, number).await?
    };
    let event: &'static str = if open { "pr.reopened" } else { "pr.closed" };
    enqueue_webhook(
        state,
        r.meta.id,
        event,
        serde_json::json!({
            "event": event, "repo": r.slug(), "sender": actor.username,
            "pull_request": { "number": pr.number, "title": pr.title },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(actor.id),
        event,
        format!(
            "PR #{} {} by {}: {}",
            pr.number,
            if open { "reopened" } else { "closed" },
            actor.username,
            pr.title
        ),
        format!("/{}/pulls/{}", r.slug(), pr.number),
    );
    index_pull(state, r, &pr).await;
    Ok(pr)
}

/// A partial PR metadata update (title/body/base/milestone).
#[derive(Default)]
pub struct PrPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub milestone: Option<i32>,
}

pub async fn update_pr(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    patch: PrPatch,
) -> Result<PullRequest, AppError> {
    let (set_milestone, milestone_id) = match patch.milestone {
        None => (false, None),
        Some(0) => (true, None),
        Some(n) => (true, Some(state.store.get_milestone_id(r.meta.id, n).await?)),
    };
    if let Some(base) = &patch.base {
        state
            .host
            .resolve_branch(r.org, r.repo, base)
            .map_err(|_| AppError::bad_request(format!("base branch not found: {base}")))?;
    }
    let title = patch.title.as_deref().map(validate_title).transpose()?;
    let pr = state
        .store
        .update_pr(
            r.meta.id,
            number,
            &UpdatePr {
                title,
                body: patch.body,
                base: patch.base,
                state: None,
                set_milestone,
                milestone_id,
            },
        )
        .await?;
    index_pull(state, r, &pr).await;
    Ok(pr)
}

/// Submit a review. Approvals only count toward branch protection when the
/// reviewer holds Write, so non-writers may comment or request changes only.
pub async fn create_review(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    reviewer: &User,
    review_state: ReviewState,
    body: Option<String>,
    comments: &[NewReviewComment],
) -> Result<Review, AppError> {
    let pr = state.store.get_pr(r.meta.id, number).await?;
    if pr.state != PrState::Open {
        return Err(AppError::conflict("pull request is not open"));
    }
    if review_state == ReviewState::Approved {
        if pr.author_id == Some(reviewer.id) {
            return Err(AppError::forbidden("you cannot approve your own pull request"));
        }
        let role = crate::authz::user_repo_role(state, reviewer, r.meta).await;
        if !matches!(role, Some(role) if role.can_write()) {
            return Err(AppError::forbidden(
                "approving requires write access; comment or request changes instead",
            ));
        }
    }
    let body = non_empty(body);
    let review = state
        .store
        .create_review(pr.id, Some(reviewer.id), review_state, body.as_deref(), comments)
        .await?;
    let title = format!(
        "{} {} PR #{number}",
        reviewer.username,
        match review_state {
            ReviewState::Approved => "approved",
            ReviewState::ChangesRequested => "requested changes on",
            ReviewState::Commented => "reviewed",
        }
    );
    enqueue_webhook(
        state,
        r.meta.id,
        "pr.reviewed",
        serde_json::json!({
            "event": "pr.reviewed", "repo": r.slug(), "sender": reviewer.username,
            "pull_request": { "number": pr.number, "title": pr.title },
            "review": { "id": review.id, "state": review_state.as_str() },
        }),
    );
    enqueue_notify(
        state,
        r,
        Some(reviewer.id),
        "review",
        title.clone(),
        format!("/{}/pulls/{number}", r.slug()),
    );
    enqueue_email(
        state,
        r,
        Some(reviewer.id),
        format!("[{}] {title}", r.slug()),
        format!(
            "A review was submitted on PR #{number} in {}.\n\nState: {}\n",
            r.slug(),
            review_state.as_str()
        ),
    );
    Ok(review)
}

/// Result of a successful merge.
#[derive(Debug, Serialize)]
pub struct MergeOutcome {
    pub merged: bool,
    pub kind: String,
    pub merge_commit_sha: String,
    pub new_base_sha: String,
    pub pull_request: PullRequest,
}

/// Merge a PR through the protected-branch gate (approvals + required checks).
/// The caller must hold Write on the repository.
pub async fn merge_pr(
    state: &AppState,
    r: &RepoRef<'_>,
    number: i32,
    merger: &User,
    strategy: Option<&str>,
    message: Option<String>,
) -> Result<MergeOutcome, AppError> {
    let pr = state.store.get_pr(r.meta.id, number).await?;
    if pr.state != PrState::Open {
        return Err(AppError::conflict("pull request is not open"));
    }

    // Pin the head to a single oid so the gate and the merge see one commit.
    let head_oid = resolve_pr_head(state, r.org, r.repo, &pr)?;

    if let Some(protection) = state
        .store
        .get_branch_protection(r.meta.id, &pr.base_branch)
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
                .required_checks_pass(r.meta.id, &head_oid.to_hex(), &protection.required_checks)
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

    let strategy_name = strategy.unwrap_or("merge");
    let git_strategy = rithub_git::MergeStrategy::parse(strategy_name);
    let message = message
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| format!("Merge pull request #{number}: {}", pr.title));
    let result = state
        .host
        .merge(
            r.org,
            r.repo,
            &pr.base_branch,
            head_oid,
            git_strategy,
            &merger.username,
            &merger.email,
            &message,
        )
        .map_err(|e| match e {
            rithub_git::GitHostError::Invalid(m) => AppError::conflict(m),
            other => AppError::from(other),
        })?;

    let merged = state
        .store
        .mark_merged(r.meta.id, number, &result.merge_commit_sha, Some(merger.id))
        .await?;

    enqueue_audit(
        state,
        Some(r.meta.id),
        Some(merger),
        "pr.merged",
        Some(&format!("#{number}")),
        Some(&format!(
            "{} into {} ({strategy_name})",
            pr.head_branch, pr.base_branch
        )),
    );
    enqueue_notify(
        state,
        r,
        Some(merger.id),
        "pr.merged",
        format!("PR #{number} merged: {}", pr.title),
        format!("/{}/pulls/{number}", r.slug()),
    );
    enqueue_email(
        state,
        r,
        Some(merger.id),
        format!("[{}] PR #{number} merged: {}", r.slug(), pr.title),
        format!(
            "Pull request #{number} was merged into {} in {} by {}.\n\nMerge commit: {}\n",
            pr.base_branch,
            r.slug(),
            merger.username,
            result.merge_commit_sha,
        ),
    );
    enqueue_webhook(
        state,
        r.meta.id,
        "pr.merged",
        serde_json::json!({
            "event": "pr.merged", "repo": r.slug(), "sender": merger.username,
            "pull_request": { "number": number, "title": pr.title },
            "merge_commit_sha": result.merge_commit_sha,
            "kind": result.kind,
        }),
    );
    // CI for the new base tip.
    crate::ci::dispatch_check_runs(
        state,
        r.meta.id,
        r.org,
        r.repo,
        &result.new_base_sha,
        &pr.base_branch,
        "push",
    );
    index_pull(state, r, &merged).await;
    crate::search_index::index_repo_git(state, r.org, r.meta).await;

    Ok(MergeOutcome {
        merged: true,
        kind: result.kind.to_string(),
        merge_commit_sha: result.merge_commit_sha,
        new_base_sha: result.new_base_sha,
        pull_request: merged,
    })
}

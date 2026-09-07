//! Web pages for issues and pull requests, including the "new" forms, the PR
//! diff + mergeability + reviews, and a merge button that posts to the same
//! logic as the JSON API (protected-branch gate included).

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{
    IssueState, IssueStore, NewIssue, NewPullRequest, PrState, PullRequestStore, Repository as RepoMeta,
    ReviewState, ReviewStore, StatusStore, Store, User,
};
use serde::Deserialize;

use super::repo::render_diff_html;
use super::{error_page, esc, page, session_user};
use crate::state::AppState;

async fn repo_or_404(
    state: &AppState,
    session: &Option<User>,
    org: &str,
    repo: &str,
) -> Result<RepoMeta, Response> {
    state
        .store
        .get_repo(org, repo)
        .await
        .map_err(|_| error_page(session, StatusCode::NOT_FOUND, "repository not found"))
}

fn tabs(org: &str, repo: &str, active: &str) -> String {
    let item = |name: &str, href: &str, key: &str| {
        let weight = if key == active { "font-weight:700" } else { "" };
        format!("<a style=\"margin-right:14px;{weight}\" href=\"{href}\">{name}</a>")
    };
    format!(
        "<div class=\"row\" style=\"border-bottom:1px solid var(--line);padding-bottom:8px;margin-bottom:12px\">{}{}{}{}</div>",
        item("Code", &format!("/{org}/{repo}"), "code"),
        item("Issues", &format!("/{org}/{repo}/issues"), "issues"),
        item("Pull requests", &format!("/{org}/{repo}/pulls"), "pulls"),
        item("Releases", &format!("/{org}/{repo}/releases"), "releases"),
    )
}

#[derive(Deserialize)]
pub struct StateQuery {
    #[serde(default)]
    state: Option<String>,
}

fn state_pill(open: bool, closed_label: &str) -> String {
    if open {
        "<span class=\"pill open\">open</span>".into()
    } else {
        format!("<span class=\"pill closed\">{closed_label}</span>")
    }
}

// --- issues -----------------------------------------------------------------

/// GET /{org}/{repo}/issues
pub async fn issues_list(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<StateQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let filter = q.state.as_deref().map(IssueState::parse);
    let issues = state.store.list_issues(meta.id, filter).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}<div class=\"row\"><a href=\"/{org}/{repo}/issues?state=open\">Open</a>· \
<a href=\"/{org}/{repo}/issues?state=closed\">Closed</a><span class=\"spacer\"></span>\
<a class=\"btn primary\" href=\"/{org}/{repo}/issues/new\">New issue</a></div></div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "issues")
    );
    body.push_str("<div class=\"card\"><table>");
    if issues.is_empty() {
        body.push_str("<tr><td class=\"muted\">No issues.</td></tr>");
    }
    for issue in issues {
        body.push_str(&format!(
            "<tr><td>{pill} <a href=\"/{org}/{repo}/issues/{n}\">#{n} {title}</a></td></tr>",
            pill = state_pill(issue.state == IssueState::Open, "closed"),
            org = esc(&org),
            repo = esc(&repo),
            n = issue.number,
            title = esc(&issue.title)
        ));
    }
    body.push_str("</table></div>");
    page("Issues", &session, &body).into_response()
}

/// GET /{org}/{repo}/issues/new
pub async fn issue_new_form(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let body = format!(
        "<div class=\"card\"><h2>New issue</h2>\
<form method=\"post\" action=\"/{org}/{repo}/issues/create\">\
<label>Title</label><input name=\"title\" required>\
<label>Description</label><textarea name=\"body\" rows=\"6\"></textarea>\
<p><button class=\"btn primary\" type=\"submit\">Create issue</button></p></form></div>",
        org = esc(&org),
        repo = esc(&repo)
    );
    page("New issue", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct IssueForm {
    title: String,
    #[serde(default)]
    body: String,
}

/// POST /{org}/{repo}/issues/create
pub async fn issue_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<IssueForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let author = session.as_ref().map(|u| u.id);
    let body = (!form.body.trim().is_empty()).then(|| form.body.clone());
    match state
        .store
        .create_issue(meta.id, author, &NewIssue { title: form.title, body })
        .await
    {
        Ok(issue) => Redirect::to(&format!("/{org}/{repo}/issues/{}", issue.number)).into_response(),
        Err(_) => error_page(&session, StatusCode::BAD_REQUEST, "could not create issue"),
    }
}

/// GET /{org}/{repo}/issues/{number}
pub async fn issue_detail(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let issue = match state.store.get_issue(meta.id, number).await {
        Ok(i) => i,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "issue not found"),
    };
    let labels = state.store.list_issue_labels(issue.id).await.unwrap_or_default();
    let assignees = state.store.list_issue_assignees(issue.id).await.unwrap_or_default();
    let comments = state.store.list_issue_comments(issue.id).await.unwrap_or_default();

    let labels_html = labels
        .iter()
        .map(|l| format!("<span class=\"pill\">{}</span> ", esc(&l.name)))
        .collect::<String>();
    let assignees_html = assignees
        .iter()
        .map(|u| esc(&u.username))
        .collect::<Vec<_>>()
        .join(", ");

    let mut body = format!(
        "<div class=\"card\"><h1>{title} <span class=\"muted\">#{n}</span></h1>\
{pill} {labels}<p class=\"muted\">assignees: {assignees}</p><div>{desc}</div></div>",
        title = esc(&issue.title),
        n = issue.number,
        pill = state_pill(issue.state == IssueState::Open, "closed"),
        labels = labels_html,
        assignees = if assignees_html.is_empty() { "—".into() } else { assignees_html },
        desc = super::markdown(issue.body.as_deref().unwrap_or(""))
    );

    body.push_str("<div class=\"card\"><h3>Comments</h3>");
    if comments.is_empty() {
        body.push_str("<p class=\"muted\">No comments.</p>");
    }
    for c in comments {
        body.push_str(&format!("<div class=\"card\">{}</div>", super::markdown(&c.body)));
    }
    body.push_str(&format!(
        "<form method=\"post\" action=\"/{org}/{repo}/issues/{n}/comment\">\
<label>Add a comment</label><textarea name=\"body\" rows=\"3\" required></textarea>\
<p><button class=\"btn\" type=\"submit\">Comment</button></p></form></div>",
        org = esc(&org),
        repo = esc(&repo),
        n = number
    ));
    page(&format!("#{number}"), &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct CommentForm {
    body: String,
}

/// POST /{org}/{repo}/issues/{number}/comment
pub async fn issue_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<CommentForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    if let Ok(issue) = state.store.get_issue(meta.id, number).await {
        let author = session.as_ref().map(|u| u.id);
        let _ = state.store.add_issue_comment(issue.id, author, &form.body).await;
    }
    Redirect::to(&format!("/{org}/{repo}/issues/{number}")).into_response()
}

// --- pull requests ----------------------------------------------------------

/// GET /{org}/{repo}/pulls
pub async fn pulls_list(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<StateQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let filter = q.state.as_deref().map(PrState::parse);
    let prs = state.store.list_prs(meta.id, filter).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}<div class=\"row\">\
<a href=\"/{org}/{repo}/pulls?state=open\">Open</a>· <a href=\"/{org}/{repo}/pulls?state=merged\">Merged</a>\
<span class=\"spacer\"></span><a class=\"btn primary\" href=\"/{org}/{repo}/pulls/new\">New pull request</a></div></div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "pulls")
    );
    body.push_str("<div class=\"card\"><table>");
    if prs.is_empty() {
        body.push_str("<tr><td class=\"muted\">No pull requests.</td></tr>");
    }
    for pr in prs {
        let pill = match pr.state {
            PrState::Open => "<span class=\"pill open\">open</span>",
            PrState::Merged => "<span class=\"pill merged\">merged</span>",
            PrState::Closed => "<span class=\"pill closed\">closed</span>",
        };
        body.push_str(&format!(
            "<tr><td>{pill} <a href=\"/{org}/{repo}/pulls/{n}\">#{n} {title}</a> \
<span class=\"muted\">{head} → {base}</span></td></tr>",
            org = esc(&org),
            repo = esc(&repo),
            n = pr.number,
            title = esc(&pr.title),
            head = esc(&pr.head_branch),
            base = esc(&pr.base_branch)
        ));
    }
    body.push_str("</table></div>");
    page("Pull requests", &session, &body).into_response()
}

/// GET /{org}/{repo}/pulls/new
pub async fn pr_new_form(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let branches = state.host.branches(&org, &repo).unwrap_or_default();
    let options = branches
        .iter()
        .map(|b| format!("<option>{}</option>", esc(b)))
        .collect::<String>();
    let body = format!(
        "<div class=\"card\"><h2>New pull request</h2>\
<form method=\"post\" action=\"/{org}/{repo}/pulls/create\">\
<label>Title</label><input name=\"title\" required>\
<label>Description</label><textarea name=\"body\" rows=\"4\"></textarea>\
<label>Compare (head)</label><select name=\"head\">{options}</select>\
<label>Base</label><select name=\"base\">{options}</select>\
<p><button class=\"btn primary\" type=\"submit\">Create pull request</button></p></form></div>",
        org = esc(&org),
        repo = esc(&repo)
    );
    page("New pull request", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct PrForm {
    title: String,
    #[serde(default)]
    body: String,
    head: String,
    base: String,
}

/// POST /{org}/{repo}/pulls/create
pub async fn pr_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<PrForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    if form.head == form.base {
        return error_page(&session, StatusCode::BAD_REQUEST, "head and base are the same");
    }
    let head_oid = state.host.resolve_branch(&org, &repo, &form.head);
    let base_oid = state.host.resolve_branch(&org, &repo, &form.base);
    let (Ok(head_oid), Ok(base_oid)) = (head_oid, base_oid) else {
        return error_page(&session, StatusCode::BAD_REQUEST, "head/base branch not found");
    };
    let author = session.as_ref().map(|u| u.id);
    let body = (!form.body.trim().is_empty()).then(|| form.body.clone());
    match state
        .store
        .create_pr(
            meta.id,
            author,
            &NewPullRequest { title: form.title, body, head: form.head, base: form.base },
            Some(&base_oid.to_hex()),
            Some(&head_oid.to_hex()),
            None,
        )
        .await
    {
        Ok(pr) => Redirect::to(&format!("/{org}/{repo}/pulls/{}", pr.number)).into_response(),
        Err(_) => error_page(&session, StatusCode::BAD_REQUEST, "could not create PR"),
    }
}

/// GET /{org}/{repo}/pulls/{number}
pub async fn pr_detail(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let pr = match state.store.get_pr(meta.id, number).await {
        Ok(p) => p,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "pull request not found"),
    };

    let pill = match pr.state {
        PrState::Open => "<span class=\"pill open\">open</span>",
        PrState::Merged => "<span class=\"pill merged\">merged</span>",
        PrState::Closed => "<span class=\"pill closed\">closed</span>",
    };
    let mut body = format!(
        "<div class=\"card\"><h1>{title} <span class=\"muted\">#{n}</span></h1>{pill} \
<span class=\"muted\">{head} → {base}</span><div>{desc}</div></div>",
        title = esc(&pr.title),
        n = pr.number,
        head = esc(&pr.head_branch),
        base = esc(&pr.base_branch),
        desc = super::markdown(pr.body.as_deref().unwrap_or(""))
    );

    // Reviews + approvals.
    let reviews = state.store.list_reviews(pr.id).await.unwrap_or_default();
    let approvals = state
        .store
        .count_current_approvals(pr.id, pr.author_id)
        .await
        .unwrap_or(0);
    let protection = state
        .store
        .get_branch_protection(meta.id, &pr.base_branch)
        .await
        .ok()
        .flatten();
    body.push_str("<div class=\"card\"><h3>Reviews</h3>");
    if reviews.is_empty() {
        body.push_str("<p class=\"muted\">No reviews yet.</p>");
    }
    for r in &reviews {
        let cls = match r.state {
            ReviewState::Approved => "ok",
            ReviewState::ChangesRequested => "bad",
            ReviewState::Commented => "muted",
        };
        body.push_str(&format!(
            "<div><span class=\"{cls}\">{}</span> {}</div>",
            r.state.as_str(),
            esc(r.body.as_deref().unwrap_or(""))
        ));
    }
    // Review submission form (needs sign-in).
    if session.is_some() {
        body.push_str(&format!(
            "<form method=\"post\" action=\"/{org}/{repo}/pulls/{n}/review\">\
<label>Submit review</label><select name=\"state\">\
<option value=\"approved\">approve</option>\
<option value=\"changes_requested\">request changes</option>\
<option value=\"commented\">comment</option></select>\
<input name=\"body\" placeholder=\"optional note\">\
<p><button class=\"btn\" type=\"submit\">Submit review</button></p></form>",
            org = esc(&org),
            repo = esc(&repo),
            n = number
        ));
    } else {
        body.push_str("<p class=\"muted\"><a href=\"/login\">Sign in</a> to review or merge.</p>");
    }
    body.push_str("</div>");

    // Mergeability + merge button.
    if pr.state == PrState::Open {
        match live_mergeability(&state, &org, &repo, &pr).await {
            Some((status, mergeable)) => {
                let required = protection.map(|p| p.required_approvals).unwrap_or(0);
                let gate_ok = approvals as i32 >= required;
                body.push_str(&format!(
                    "<div class=\"card\"><h3>Merge</h3><p>Status: <strong>{status}</strong> · \
approvals: {approvals}{req}</p>",
                    req = if required > 0 { format!(" / {required} required") } else { String::new() }
                ));
                if mergeable && gate_ok && session.is_some() {
                    body.push_str(&format!(
                        "<form method=\"post\" action=\"/{org}/{repo}/pulls/{n}/merge\">\
<button class=\"btn primary\" type=\"submit\">Merge pull request</button></form>",
                        org = esc(&org),
                        repo = esc(&repo),
                        n = number
                    ));
                } else if !mergeable {
                    body.push_str("<p class=\"bad\">Not mergeable (conflicts or nothing to merge).</p>");
                } else if !gate_ok {
                    body.push_str(&format!(
                        "<p class=\"bad\">Blocked: needs {required} approval(s), has {approvals}.</p>"
                    ));
                } else {
                    body.push_str("<p class=\"muted\"><a href=\"/login\">Sign in</a> to merge.</p>");
                }
                body.push_str("</div>");
            }
            None => body.push_str("<div class=\"card\"><p class=\"bad\">Could not compute mergeability.</p></div>"),
        }
    } else if let Some(sha) = &pr.merge_commit_sha {
        body.push_str(&format!(
            "<div class=\"card\"><p class=\"ok\">Merged as <a href=\"/{org}/{repo}/commit/{sha}\">{short}</a></p></div>",
            org = esc(&org),
            repo = esc(&repo),
            sha = esc(sha),
            short = esc(&sha[..sha.len().min(8)])
        ));
    }

    // The diff.
    if let (Ok(base), Ok(head)) = (
        state.host.resolve_branch(&org, &repo, &pr.base_branch),
        state.host.resolve_branch(&org, &repo, &pr.head_branch),
    ) {
        if let Ok(diff) = state.host.pr_diff(&org, &repo, base, head) {
            body.push_str("<h3>Diff</h3>");
            body.push_str(&render_diff_html(&diff));
        }
    }

    page(&format!("PR #{number}"), &session, &body).into_response()
}

async fn live_mergeability(
    state: &AppState,
    org: &str,
    repo: &str,
    pr: &rithub_core::PullRequest,
) -> Option<(String, bool)> {
    let base = state.host.resolve_branch(org, repo, &pr.base_branch).ok()?;
    let head = state.host.resolve_branch(org, repo, &pr.head_branch).ok()?;
    let analysis = state.host.mergeability(org, repo, base, head).ok()?;
    Some((analysis.status.as_str().to_string(), analysis.status.mergeable()))
}

#[derive(Deserialize)]
pub struct ReviewForm {
    state: String,
    #[serde(default)]
    body: String,
}

/// POST /{org}/{repo}/pulls/{number}/review
pub async fn pr_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<ReviewForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let Some(user) = session.as_ref() else {
        return error_page(&session, StatusCode::UNAUTHORIZED, "sign in to review");
    };
    if let Ok(pr) = state.store.get_pr(meta.id, number).await {
        if let Some(review_state) = ReviewState::parse(&form.state) {
            let note = (!form.body.trim().is_empty()).then(|| form.body.clone());
            let _ = state
                .store
                .create_review(pr.id, Some(user.id), review_state, note.as_deref(), &[])
                .await;
        }
    }
    Redirect::to(&format!("/{org}/{repo}/pulls/{number}")).into_response()
}

/// POST /{org}/{repo}/pulls/{number}/merge
pub async fn pr_merge(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let Some(user) = session.clone() else {
        return error_page(&session, StatusCode::UNAUTHORIZED, "sign in to merge");
    };
    let pr = match state.store.get_pr(meta.id, number).await {
        Ok(p) if p.state == PrState::Open => p,
        Ok(_) => return error_page(&session, StatusCode::CONFLICT, "pull request is not open"),
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "pull request not found"),
    };

    // Pin the head to a single oid so the gate and the merge act on the same
    // commit (no TOCTOU window for a push landing between the two).
    let head_oid = match state.host.resolve_branch(&org, &repo, &pr.head_branch) {
        Ok(oid) => oid,
        Err(e) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    // Protected-branch gate (same as the JSON API): approvals + required checks.
    if let Ok(Some(protection)) = state.store.get_branch_protection(meta.id, &pr.base_branch).await {
        let approvals = state
            .store
            .count_current_approvals(pr.id, pr.author_id)
            .await
            .unwrap_or(0);
        if approvals < protection.required_approvals as i64 {
            return error_page(
                &session,
                StatusCode::FORBIDDEN,
                &format!(
                    "branch '{}' requires {} approval(s); have {}",
                    pr.base_branch, protection.required_approvals, approvals
                ),
            );
        }
        if !protection.required_checks.is_empty() {
            let pass = state
                .store
                .required_checks_pass(meta.id, &head_oid.to_hex(), &protection.required_checks)
                .await
                .unwrap_or(false);
            if !pass {
                return error_page(
                    &session,
                    StatusCode::FORBIDDEN,
                    &format!(
                        "branch '{}' requires status checks: {}",
                        pr.base_branch,
                        protection.required_checks.join(", ")
                    ),
                );
            }
        }
    }

    let message = format!("Merge pull request #{number}: {}", pr.title);
    match state.host.merge(
        &org,
        &repo,
        &pr.base_branch,
        head_oid,
        rithub_git::MergeStrategy::Merge,
        &user.username,
        &user.email,
        &message,
    ) {
        Ok(result) => {
            let _ = state
                .store
                .mark_merged(meta.id, number, &result.merge_commit_sha, Some(user.id))
                .await;
            crate::audit::record(
                &state,
                Some(meta.id),
                Some(user.id),
                &user.username,
                "pr.merged",
                Some(&format!("#{number}")),
                Some("via web"),
            )
            .await;
            Redirect::to(&format!("/{org}/{repo}/pulls/{number}")).into_response()
        }
        Err(rithub_git::GitHostError::Invalid(m)) => {
            error_page(&session, StatusCode::CONFLICT, &m)
        }
        Err(_) => error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "merge failed"),
    }
}

//! Pull-request pages: list, new (same-repo or from a fork), and the detail page
//! with checks, reviews (with reviewer names and inline comments), the
//! conversation, close/reopen, and a merge box with a strategy picker.
//!
//! Every mutation goes through [`crate::collab_svc`] — the same gate (approvals
//! + required checks) and side effects (webhooks, notifications, audit, CI,
//! search) as the JSON API. Merging requires Write; approving requires Write
//! and not being the author.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use rithub_core::{
    CiStore, DirectoryStore, PrState, PullRequestStore, RepoRole, ReviewState, ReviewStore, Store,
    User,
};
use serde::Deserialize;

use super::collab::comment_div;
use super::repo::render_diff_html;
use super::{
    app_error_page, check_runs_html, error_page, esc, is_htmx, markdown, notice_html, page,
    repo_header, session_user, short_date, username_of, web_repo_participant, web_repo_read,
    web_repo_role,
};
use crate::collab_svc::{self as svc, PrInput, RepoRef};
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct PrQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    notice: Option<String>,
    #[serde(default)]
    head_repo: Option<String>,
}

fn pr_pill(state: PrState) -> &'static str {
    match state {
        PrState::Open => "<span class=\"pill open\">Open</span>",
        PrState::Merged => "<span class=\"pill merged\">Merged</span>",
        PrState::Closed => "<span class=\"pill closed\">Closed</span>",
    }
}

/// GET /{org}/{repo}/pulls
pub async fn pulls_list(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<PrQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let active = q.state.as_deref().unwrap_or("open");
    let prs = state
        .store
        .list_prs(meta.id, Some(PrState::parse(active)))
        .await
        .unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let bold = |k: &str| if k == active { " style=\"font-weight:700\"" } else { "" };
    let mut body = repo_header(&org, &repo, &meta, "pulls", role);
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row\">\
         <a href=\"/{o}/{r}/pulls?state=open\"{b1}>Open</a>\
         <a href=\"/{o}/{r}/pulls?state=merged\"{b2}>Merged</a>\
         <a href=\"/{o}/{r}/pulls?state=closed\"{b3}>Closed</a>\
         <span class=\"spacer\"></span>\
         <a class=\"btn primary\" href=\"/{o}/{r}/pulls/new\">New pull request</a></div><table>",
        b1 = bold("open"),
        b2 = bold("merged"),
        b3 = bold("closed"),
    ));
    if prs.is_empty() {
        body.push_str("<tr><td class=\"muted\" style=\"padding:20px 8px\">No pull requests matching this filter.</td></tr>");
    }
    for pr in prs {
        body.push_str(&format!(
            "<tr><td>{pill} <a href=\"/{o}/{r}/pulls/{n}\"><strong>{title}</strong></a><br>\
             <span class=\"muted\">#{n} \u{00b7} {head} \u{2192} {base} \u{00b7} opened {d} by {who}</span></td></tr>",
            pill = pr_pill(pr.state),
            n = pr.number,
            title = esc(&pr.title),
            head = esc(&pr.head_branch),
            base = esc(&pr.base_branch),
            d = short_date(pr.created_at.unix_timestamp()),
            who = esc(&username_of(&state, pr.author_id).await),
        ));
    }
    body.push_str("</table></div>");
    page("Pull requests", &session, &body).into_response()
}

/// GET /{org}/{repo}/pulls/new — choose head repository (this repo or a fork
/// the user can read) and branches.
pub async fn pr_new_form(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<PrQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/new");
    let (meta, user, role) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let self_slug = format!("{org}/{repo}");
    // Candidate head repos: this repo + readable forks.
    let mut head_repos = vec![self_slug.clone()];
    for (fo, fr, _) in state.store.list_forks(meta.id).await.unwrap_or_default() {
        if let Ok(fmeta) = state.store.get_repo(&fo, &fr).await {
            if crate::authz::user_can_read(&state, Some(&user), &fmeta).await {
                head_repos.push(format!("{fo}/{fr}"));
            }
        }
    }
    let selected = q
        .head_repo
        .clone()
        .filter(|h| head_repos.contains(h))
        .unwrap_or_else(|| self_slug.clone());
    let (ho, hr) = selected.split_once('/').unwrap_or((&org, &repo));
    let base_branches = state.host.branches(&org, &repo).unwrap_or_default();
    let head_branches = state.host.branches(ho, hr).unwrap_or_default();
    let default_branch = state.host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());

    let opts = |list: &[String], sel: &str| -> String {
        list.iter()
            .map(|b| {
                let s = if b == sel { " selected" } else { "" };
                format!("<option value=\"{v}\"{s}>{v}</option>", v = esc(b))
            })
            .collect()
    };
    let repo_opts: String = head_repos
        .iter()
        .map(|h| {
            let s = if *h == selected { " selected" } else { "" };
            format!("<option value=\"{v}\"{s}>{v}</option>", v = esc(h))
        })
        .collect();
    let head_default = head_branches
        .iter()
        .find(|b| selected != self_slug || **b != default_branch)
        .cloned()
        .unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let mut body = repo_header(&org, &repo, &meta, "pulls", role);
    if base_branches.is_empty() {
        body.push_str("<div class=\"card\"><p class=\"muted\">This repository has no branches yet. Push a branch first.</p></div>");
        return page("New pull request", &session, &body).into_response();
    }
    body.push_str(&format!(
        "<div class=\"card\"><h2>Open a pull request</h2>\
         <form method=\"get\" action=\"/{o}/{r}/pulls/new\" class=\"row\">\
         <label for=\"pr-hr\" style=\"margin:0\">Head repository</label>\
         <select id=\"pr-hr\" name=\"head_repo\" style=\"width:auto\" onchange=\"this.form.submit()\">{repo_opts}</select>\
         <noscript><button class=\"btn small\" type=\"submit\">Load branches</button></noscript></form>\
         <form method=\"post\" action=\"/{o}/{r}/pulls/create\">\
         <input type=\"hidden\" name=\"head_repo\" value=\"{sel}\">\
         <div class=\"grid2\"><div><label for=\"pr-base\">Base branch (into {o}/{r})</label>\
         <select id=\"pr-base\" name=\"base\">{base_opts}</select></div>\
         <div><label for=\"pr-head\">Compare branch (from {sel})</label>\
         <select id=\"pr-head\" name=\"head\">{head_opts}</select></div></div>\
         <label for=\"pr-title\">Title</label>\
         <input id=\"pr-title\" name=\"title\" required maxlength=\"256\" placeholder=\"Describe the change\">\
         <label for=\"pr-body\">Description</label>\
         <textarea id=\"pr-body\" name=\"body\" rows=\"6\" placeholder=\"What does this change do and why? Markdown supported.\"></textarea>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Create pull request</button></p>\
         </form></div>",
        sel = esc(&selected),
        base_opts = opts(&base_branches, &default_branch),
        head_opts = opts(&head_branches, &head_default),
    ));
    page("New pull request", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct PrForm {
    title: String,
    #[serde(default)]
    body: String,
    head: String,
    base: String,
    #[serde(default)]
    head_repo: Option<String>,
}

/// POST /{org}/{repo}/pulls/create
pub async fn pr_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<PrForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/new");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let input = PrInput {
        title: form.title,
        body: Some(form.body),
        head: form.head,
        base: form.base,
        head_repo: form.head_repo,
    };
    match svc::create_pr(&state, &r, &user, input).await {
        Ok(pr) => Redirect::to(&format!("/{org}/{repo}/pulls/{}", pr.number)).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// Render the reviews list (also the htmx partial after a review submit).
async fn reviews_html(state: &AppState, pr: &rithub_core::PullRequest) -> String {
    let reviews = state.store.list_reviews(pr.id).await.unwrap_or_default();
    let comments = state.store.list_review_comments(pr.id).await.unwrap_or_default();
    let approvals = state
        .store
        .count_current_approvals(pr.id, pr.author_id)
        .await
        .unwrap_or(0);
    let mut out = format!("<div id=\"pr-reviews\"><p class=\"muted\">{approvals} current approval(s)</p>");
    if reviews.is_empty() {
        out.push_str("<p class=\"muted\">No reviews yet.</p>");
    }
    for review in &reviews {
        let (cls, verb) = match review.state {
            ReviewState::Approved => ("ok", "approved these changes"),
            ReviewState::ChangesRequested => ("bad", "requested changes"),
            ReviewState::Commented => ("muted", "reviewed"),
        };
        let inline: String = comments
            .iter()
            .filter(|c| c.review_id == review.id)
            .map(|c| {
                format!(
                    "<div style=\"margin-top:6px\"><span class=\"mono muted\">{p}{l}</span><div class=\"readme\">{b}</div></div>",
                    p = esc(&c.path),
                    l = c.line.map(|l| format!(":{l}")).unwrap_or_default(),
                    b = markdown(&c.body)
                )
            })
            .collect();
        out.push_str(&format!(
            "<div class=\"comment\"><div class=\"comment-h\"><strong>{who}</strong> \
             <span class=\"{cls}\">{verb}</span> <span class=\"muted\">on {d}</span></div>\
             <div class=\"comment-b\">{body}{inline}</div></div>",
            who = esc(&username_of(state, review.reviewer_id).await),
            d = short_date(review.created_at.unix_timestamp()),
            body = review.body.as_deref().map(markdown).unwrap_or_default(),
        ));
    }
    out.push_str("</div>");
    out
}

/// GET /{org}/{repo}/pulls/{number}
pub async fn pr_detail(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<PrQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let pr = match state.store.get_pr(meta.id, number).await {
        Ok(p) => p,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "pull request not found"),
    };
    let can_write = role.is_some_and(|r| r.can_write());
    let is_author = session.as_ref().is_some_and(|u| Some(u.id) == pr.author_id);
    let (o, r) = (esc(&org), esc(&repo));
    let head_label = match pr.head_repo_id {
        Some(_) => format!("fork:{}", pr.head_branch),
        None => pr.head_branch.clone(),
    };

    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "pulls", role));
    body.push_str(&format!(
        "<div class=\"card\"><h1>{title} <span class=\"muted\">#{n}</span></h1>\
         <p>{pill} <span class=\"muted\">{who} wants to merge <code>{head}</code> into <code>{base}</code> \u{00b7} opened {d}</span></p></div>",
        title = esc(&pr.title),
        n = pr.number,
        pill = pr_pill(pr.state),
        who = esc(&username_of(&state, pr.author_id).await),
        head = esc(&head_label),
        base = esc(&pr.base_branch),
        d = short_date(pr.created_at.unix_timestamp()),
    ));

    // Conversation: description + comments.
    body.push_str(&comment_div(
        &username_of(&state, pr.author_id).await,
        pr.created_at.unix_timestamp(),
        pr.body.as_deref().unwrap_or("*No description provided.*"),
    ));
    body.push_str("<div id=\"pr-comments\">");
    for c in state.store.list_pr_comments(pr.id).await.unwrap_or_default() {
        body.push_str(&comment_div(
            &username_of(&state, c.author_id).await,
            c.created_at.unix_timestamp(),
            &c.body,
        ));
    }
    body.push_str("</div>");

    // Checks for the pinned head.
    if let Some(head_sha) = &pr.head_sha {
        let runs = state.store.latest_check_runs(meta.id, head_sha).await.unwrap_or_default();
        body.push_str("<div class=\"card\"><h3>Checks</h3>");
        body.push_str(&check_runs_html(&org, &repo, &runs));
        body.push_str("</div>");
    }

    // Reviews + review form.
    body.push_str("<div class=\"card\"><h3>Reviews</h3>");
    body.push_str(&reviews_html(&state, &pr).await);
    if session.is_some() && pr.state == PrState::Open {
        let action = format!("/{o}/{r}/pulls/{number}/review");
        let approve = if can_write && !is_author {
            "<option value=\"approved\">Approve</option>"
        } else {
            ""
        };
        body.push_str(&format!(
            "<form method=\"post\" action=\"{action}\" hx-post=\"{action}\" hx-target=\"#pr-reviews\" \
             hx-swap=\"outerHTML\" hx-on::after-request=\"if(event.detail.successful)this.reset()\" style=\"margin-top:12px\">\
             <label for=\"rv-body\">Submit a review</label>\
             <textarea id=\"rv-body\" name=\"body\" rows=\"3\" placeholder=\"Optional review summary\"></textarea>\
             <div class=\"row\" style=\"margin-top:6px\"><select name=\"state\" style=\"width:auto\" aria-label=\"Review verdict\">\
             <option value=\"commented\">Comment</option>{approve}\
             <option value=\"changes_requested\">Request changes</option></select>\
             <button class=\"btn\" type=\"submit\">Submit review</button></div></form>"
        ));
    }
    body.push_str("</div>");

    // Merge box.
    if pr.state == PrState::Open {
        body.push_str(&merge_box(&state, &org, &repo, &meta, &pr, can_write, &session).await);
    } else if let Some(sha) = &pr.merge_commit_sha {
        body.push_str(&format!(
            "<div class=\"card\"><p class=\"ok\">Merged as <a href=\"/{o}/{r}/commit/{sha}\" class=\"mono\">{short}</a></p></div>",
            sha = esc(sha),
            short = esc(&sha[..sha.len().min(8)])
        ));
    }

    // Comment + close/reopen.
    if session.is_some() {
        let action = format!("/{o}/{r}/pulls/{number}/comment");
        body.push_str(&format!(
            "<div class=\"card\"><form method=\"post\" action=\"{action}\" hx-post=\"{action}\" \
             hx-target=\"#pr-comments\" hx-swap=\"beforeend\" hx-on::after-request=\"if(event.detail.successful)this.reset()\">\
             <label for=\"prc-body\">Add a comment</label>\
             <textarea id=\"prc-body\" name=\"body\" rows=\"4\" required placeholder=\"Markdown supported\"></textarea>\
             <p style=\"margin-top:8px\"><button class=\"btn primary\" type=\"submit\">Comment</button></p></form>"
        ));
        if (can_write || is_author) && pr.state != PrState::Merged {
            let (label, target) = if pr.state == PrState::Open {
                ("Close pull request", "closed")
            } else {
                ("Reopen pull request", "open")
            };
            body.push_str(&format!(
                "<form method=\"post\" action=\"/{o}/{r}/pulls/{number}/state\">\
                 <input type=\"hidden\" name=\"state\" value=\"{target}\">\
                 <button class=\"btn\" type=\"submit\">{label}</button></form>"
            ));
        }
        body.push_str("</div>");
    } else {
        body.push_str(&format!(
            "<p class=\"muted\"><a href=\"/login?next=/{o}/{r}/pulls/{number}\">Sign in</a> to comment or review.</p>"
        ));
    }

    // The diff (base tip vs. pinned/live head, including fork PRs).
    match svc::pr_diff(&state, &org, &repo, &pr) {
        Ok(diff) => {
            body.push_str("<h3>Files changed</h3>");
            body.push_str(&render_diff_html(&diff));
        }
        Err(_) if pr.state != PrState::Open => {}
        Err(_) => body.push_str("<div class=\"card\"><p class=\"muted\">The diff is unavailable (a branch may have been deleted).</p></div>"),
    }

    page(&format!("PR #{number} {}", pr.title), &session, &body).into_response()
}

async fn merge_box(
    state: &AppState,
    org: &str,
    repo: &str,
    meta: &rithub_core::Repository,
    pr: &rithub_core::PullRequest,
    can_write: bool,
    session: &Option<User>,
) -> String {
    let Ok(m) = svc::current_mergeability(state, org, repo, pr) else {
        return "<div class=\"card\"><p class=\"bad\">Could not compute mergeability (a branch may be missing).</p></div>".into();
    };
    let approvals = state.store.count_current_approvals(pr.id, pr.author_id).await.unwrap_or(0);
    let protection = state
        .store
        .get_branch_protection(meta.id, &pr.base_branch)
        .await
        .ok()
        .flatten();
    let required = protection.as_ref().map(|p| p.required_approvals).unwrap_or(0) as i64;
    let checks_ok = match &protection {
        Some(p) if !p.required_checks.is_empty() => {
            use rithub_core::StatusStore;
            state
                .store
                .required_checks_pass(meta.id, &m.head_sha, &p.required_checks)
                .await
                .unwrap_or(false)
        }
        _ => true,
    };
    let mut out = format!(
        "<div class=\"card\"><h3>Merge</h3><p>Mergeability: <strong>{status}</strong> \u{00b7} Approvals: {approvals}{req}{checks}</p>",
        status = esc(&m.status),
        req = if required > 0 { format!(" / {required} required") } else { String::new() },
        checks = match &protection {
            Some(p) if !p.required_checks.is_empty() => format!(
                " \u{00b7} Required checks ({}): <span class=\"{}\">{}</span>",
                esc(&p.required_checks.join(", ")),
                if checks_ok { "ok" } else { "bad" },
                if checks_ok { "passing" } else { "not passing" }
            ),
            _ => String::new(),
        },
    );
    if !m.mergeable {
        out.push_str("<p class=\"bad\">This branch has conflicts or nothing to merge.</p>");
    } else if approvals < required {
        out.push_str(&format!("<p class=\"bad\">Blocked: needs {required} approval(s), has {approvals}.</p>"));
    } else if !checks_ok {
        out.push_str("<p class=\"bad\">Blocked: required status checks have not passed.</p>");
    } else if can_write {
        out.push_str(&format!(
            "<form method=\"post\" action=\"/{o}/{r}/pulls/{n}/merge\">\
             <div class=\"row\"><select name=\"strategy\" style=\"width:auto\" aria-label=\"Merge strategy\">\
             <option value=\"merge\">Create a merge commit</option>\
             <option value=\"squash\">Squash and merge</option>\
             <option value=\"rebase\">Rebase and merge</option></select>\
             <input name=\"message\" placeholder=\"Commit message (optional)\" style=\"flex:1\" aria-label=\"Commit message\">\
             <button class=\"btn primary\" type=\"submit\">Merge pull request</button></div></form>",
            o = esc(org),
            r = esc(repo),
            n = pr.number
        ));
    } else if session.is_some() {
        out.push_str("<p class=\"muted\">Only collaborators with write access can merge.</p>");
    } else {
        out.push_str("<p class=\"muted\"><a href=\"/login\">Sign in</a> to merge.</p>");
    }
    out.push_str("</div>");
    out
}

#[derive(Deserialize)]
pub struct ReviewForm {
    state: String,
    #[serde(default)]
    body: String,
}

/// POST /{org}/{repo}/pulls/{number}/review — htmx gets the refreshed review
/// list (or an error fragment); plain forms redirect.
pub async fn pr_review(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<ReviewForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/{number}");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(review_state) = ReviewState::parse(&form.state) else {
        return error_page(&session, StatusCode::BAD_REQUEST, "unknown review state");
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let result = svc::create_review(&state, &r, number, &user, review_state, Some(form.body), &[]).await;
    if is_htmx(&headers) {
        let pr = match state.store.get_pr(meta.id, number).await {
            Ok(p) => p,
            Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "pull request not found"),
        };
        let list = reviews_html(&state, &pr).await;
        return match result {
            Ok(_) => Html(list).into_response(),
            Err(e) => Html(list.replacen(
                "<div id=\"pr-reviews\">",
                &format!(
                    "<div id=\"pr-reviews\"><div class=\"notice bad-notice\">{}</div>",
                    esc(&e.client_message())
                ),
                1,
            ))
            .into_response(),
        };
    }
    match result {
        Ok(_) => Redirect::to(&next).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct CommentForm {
    body: String,
}

/// POST /{org}/{repo}/pulls/{number}/comment
pub async fn pr_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<CommentForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/{number}");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::comment_pr(&state, &r, number, &user, &form.body).await {
        Ok(c) if is_htmx(&headers) => {
            Html(comment_div(&user.username, c.created_at.unix_timestamp(), &c.body)).into_response()
        }
        Ok(_) => Redirect::to(&next).into_response(),
        Err(e) if is_htmx(&headers) => (
            e.status(),
            Html(format!("<div class=\"notice bad-notice\">{}</div>", esc(&e.client_message()))),
        )
            .into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct StateForm {
    state: String,
}

/// POST /{org}/{repo}/pulls/{number}/state — close/reopen (author or Write).
pub async fn pr_state(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<StateForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/{number}");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let pr = match state.store.get_pr(meta.id, number).await {
        Ok(p) => p,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "pull request not found"),
    };
    if !svc::is_author_or_role(&state, &user, &meta, pr.author_id, RepoRole::Write).await {
        return error_page(&session, StatusCode::FORBIDDEN, "only the author or a writer can change this pull request");
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::set_pr_state(&state, &r, number, &user, form.state == "open").await {
        Ok(_) => Redirect::to(&next).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct MergeForm {
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// POST /{org}/{repo}/pulls/{number}/merge (Write)
pub async fn pr_merge(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<MergeForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/pulls/{number}");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let strategy = match form.strategy.as_deref() {
        Some(s @ ("merge" | "squash" | "rebase")) => Some(s),
        None => None,
        Some(_) => return error_page(&session, StatusCode::BAD_REQUEST, "unknown merge strategy"),
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::merge_pr(&state, &r, number, &user, strategy, form.message).await {
        Ok(_) => Redirect::to(&next).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

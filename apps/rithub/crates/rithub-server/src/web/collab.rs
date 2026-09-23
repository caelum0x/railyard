//! Issue, label, and milestone pages.
//!
//! Every mutation goes through [`crate::collab_svc`] (the same code the JSON API
//! uses), so an issue opened, commented on, closed, or triaged in the browser
//! fires the same webhooks, notifications, and search indexing as the API.
//!
//! ## Permissions
//! - reading follows repository visibility;
//! - opening and commenting need a signed-in reader;
//! - closing/reopening needs the issue author or Write;
//! - labels, assignees, milestones, and label/milestone management need Write.
//!
//! ## HTMX
//! The comment form posts with `hx-post` and appends the rendered comment to
//! `#issue-comments` (`hx-swap="beforeend"`); without JS it is a normal POST +
//! redirect.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use rithub_core::{
    DirectoryStore, IssueState, IssueStore, MilestoneStore, NewMilestone, RepoRole, UpdateMilestone,
};
use serde::Deserialize;

use super::{
    app_error_page, error_page, esc, is_htmx, label_chip, markdown, notice_html, page, repo_header,
    session_user, short_date, username_of, web_repo_participant, web_repo_read, web_repo_role,
};
use crate::collab_svc::{self as svc, IssuePatch, LabelSpec, RepoRef};
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct StateQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    notice: Option<String>,
}

fn state_pill(open: bool) -> &'static str {
    if open {
        "<span class=\"pill open\">Open</span>"
    } else {
        "<span class=\"pill closed\">Closed</span>"
    }
}

/// A rendered comment block (full page and HTMX partial share this).
pub fn comment_div(author: &str, date: i64, body_md: &str) -> String {
    format!(
        "<div class=\"comment\"><div class=\"comment-h\"><strong>{a}</strong> \
         <span class=\"muted\">commented on {d}</span></div>\
         <div class=\"comment-b readme\">{b}</div></div>",
        a = esc(author),
        d = short_date(date),
        b = markdown(body_md)
    )
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
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let active = q.state.as_deref().unwrap_or("open");
    let issues = state
        .store
        .list_issues(meta.id, Some(IssueState::parse(active)))
        .await
        .unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let bold = |k: &str| if k == active { " style=\"font-weight:700\"" } else { "" };
    let mut body = repo_header(&org, &repo, &meta, "issues", role);
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row\">\
         <a href=\"/{o}/{r}/issues?state=open\"{b1}>Open</a>\
         <a href=\"/{o}/{r}/issues?state=closed\"{b2}>Closed</a>\
         <a href=\"/{o}/{r}/labels\">Labels</a><a href=\"/{o}/{r}/milestones\">Milestones</a>\
         <span class=\"spacer\"></span>\
         <a class=\"btn primary\" href=\"/{o}/{r}/issues/new\">New issue</a></div><table>",
        b1 = bold("open"),
        b2 = bold("closed"),
    ));
    if issues.is_empty() {
        body.push_str("<tr><td class=\"muted\" style=\"padding:20px 8px\">No issues matching this filter.</td></tr>");
    }
    for issue in issues {
        let labels: String = state
            .store
            .list_issue_labels(issue.id)
            .await
            .unwrap_or_default()
            .iter()
            .map(|l| label_chip(&l.name, &l.color))
            .collect::<Vec<_>>()
            .join(" ");
        body.push_str(&format!(
            "<tr><td>{pill} <a href=\"/{o}/{r}/issues/{n}\"><strong>{title}</strong></a> {labels}<br>\
             <span class=\"muted\">#{n} opened {d} by {who}</span></td></tr>",
            pill = state_pill(issue.state == IssueState::Open),
            n = issue.number,
            title = esc(&issue.title),
            d = short_date(issue.created_at.unix_timestamp()),
            who = esc(&username_of(&state, issue.author_id).await),
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
    let next = format!("/{org}/{repo}/issues/new");
    let (meta, _, role) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut body = repo_header(&org, &repo, &meta, "issues", role);
    body.push_str(&format!(
        "<div class=\"card\"><h2>New issue</h2>\
         <form method=\"post\" action=\"/{o}/{r}/issues/create\">\
         <label for=\"i-title\">Title</label>\
         <input id=\"i-title\" name=\"title\" required maxlength=\"256\" placeholder=\"Short description of the problem\">\
         <label for=\"i-body\">Description</label>\
         <textarea id=\"i-body\" name=\"body\" rows=\"8\" placeholder=\"Steps to reproduce, expected and actual behaviour. Markdown supported.\"></textarea>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Submit new issue</button></p>\
         </form></div>",
        o = esc(&org),
        r = esc(&repo)
    ));
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
    let next = format!("/{org}/{repo}/issues/new");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::create_issue(&state, &r, &user, &form.title, Some(form.body)).await {
        Ok(issue) => Redirect::to(&format!("/{org}/{repo}/issues/{}", issue.number)).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// GET /{org}/{repo}/issues/{number}
pub async fn issue_detail(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<StateQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let issue = match state.store.get_issue(meta.id, number).await {
        Ok(i) => i,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "issue not found"),
    };
    let labels = state.store.list_issue_labels(issue.id).await.unwrap_or_default();
    let assignees = state.store.list_issue_assignees(issue.id).await.unwrap_or_default();
    let comments = state.store.list_issue_comments(issue.id).await.unwrap_or_default();
    let can_write = role.is_some_and(|r| r.can_write());
    let is_author = session.as_ref().is_some_and(|u| Some(u.id) == issue.author_id);
    let (o, r) = (esc(&org), esc(&repo));
    let open = issue.state == IssueState::Open;

    let milestone = match issue.milestone_id {
        Some(mid) => state
            .store
            .list_milestones(meta.id)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.milestone.id == mid)
            .map(|m| (m.milestone.number, m.milestone.title)),
        None => None,
    };

    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "issues", role));
    body.push_str(&format!(
        "<div class=\"card\"><h1>{title} <span class=\"muted\">#{n}</span></h1>\
         <p>{pill} <span class=\"muted\">opened {d} by {who}</span></p></div>",
        title = esc(&issue.title),
        n = issue.number,
        pill = state_pill(open),
        d = short_date(issue.created_at.unix_timestamp()),
        who = esc(&username_of(&state, issue.author_id).await),
    ));

    body.push_str("<div class=\"grid2\" style=\"grid-template-columns:3fr 1fr\"><div>");
    body.push_str(&comment_div(
        &username_of(&state, issue.author_id).await,
        issue.created_at.unix_timestamp(),
        issue.body.as_deref().unwrap_or("*No description provided.*"),
    ));
    body.push_str("<div id=\"issue-comments\">");
    for c in &comments {
        body.push_str(&comment_div(
            &username_of(&state, c.author_id).await,
            c.created_at.unix_timestamp(),
            &c.body,
        ));
    }
    body.push_str("</div>");

    if session.is_some() {
        let action = format!("/{o}/{r}/issues/{number}/comment");
        body.push_str(&format!(
            "<div class=\"card\"><form method=\"post\" action=\"{action}\" hx-post=\"{action}\" \
             hx-target=\"#issue-comments\" hx-swap=\"beforeend\" hx-on::after-request=\"if(event.detail.successful)this.reset()\">\
             <label for=\"c-body\">Add a comment</label>\
             <textarea id=\"c-body\" name=\"body\" rows=\"4\" required placeholder=\"Markdown supported\"></textarea>\
             <div class=\"row\" style=\"margin-top:8px\"><button class=\"btn primary\" type=\"submit\">Comment</button>\
             </div></form>"
        ));
        if can_write || is_author {
            let (label, target) = if open { ("Close issue", "closed") } else { ("Reopen issue", "open") };
            body.push_str(&format!(
                "<form method=\"post\" action=\"/{o}/{r}/issues/{number}/state\" style=\"margin-top:8px\">\
                 <input type=\"hidden\" name=\"state\" value=\"{target}\">\
                 <button class=\"btn\" type=\"submit\">{label}</button></form>"
            ));
        }
        body.push_str("</div>");
    } else {
        body.push_str(&format!(
            "<p class=\"muted\"><a href=\"/login?next=/{o}/{r}/issues/{number}\">Sign in</a> to comment.</p>"
        ));
    }
    body.push_str("</div><div>");

    // Sidebar: labels, assignees, milestone (editable by writers).
    let labels_html = if labels.is_empty() {
        "<span class=\"muted\">None yet</span>".to_string()
    } else {
        labels.iter().map(|l| label_chip(&l.name, &l.color)).collect::<Vec<_>>().join(" ")
    };
    let assignees_html = if assignees.is_empty() {
        "<span class=\"muted\">No one assigned</span>".to_string()
    } else {
        assignees
            .iter()
            .map(|u| format!("<a href=\"/{u}\">{u}</a>", u = esc(&u.username)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let milestone_html = milestone
        .as_ref()
        .map(|(_, t)| esc(t))
        .unwrap_or_else(|| "<span class=\"muted\">No milestone</span>".into());
    body.push_str(&format!(
        "<div class=\"card\"><h3>Labels</h3><p>{labels_html}</p>\
         <h3>Assignees</h3><p>{assignees_html}</p>\
         <h3>Milestone</h3><p>{milestone_html}</p></div>"
    ));
    if can_write {
        let all_labels = state.store.list_labels(meta.id).await.unwrap_or_default();
        let milestones = state.store.list_milestones(meta.id).await.unwrap_or_default();
        let label_boxes: String = all_labels
            .iter()
            .map(|l| {
                let checked = if labels.iter().any(|x| x.id == l.id) { " checked" } else { "" };
                format!(
                    "<label class=\"check\"><input type=\"checkbox\" name=\"label\" value=\"{n}\"{checked}> {chip}</label>",
                    n = esc(&l.name),
                    chip = label_chip(&l.name, &l.color)
                )
            })
            .collect();
        let current_ms = milestone.as_ref().map(|(n, _)| *n).unwrap_or(0);
        let ms_options: String = std::iter::once(format!(
            "<option value=\"0\"{}>No milestone</option>",
            if current_ms == 0 { " selected" } else { "" }
        ))
        .chain(milestones.iter().filter(|m| m.milestone.closed_at.is_none() || m.milestone.number == current_ms).map(|m| {
            format!(
                "<option value=\"{n}\"{sel}>{t}</option>",
                n = m.milestone.number,
                sel = if m.milestone.number == current_ms { " selected" } else { "" },
                t = esc(&m.milestone.title)
            )
        }))
        .collect();
        let assignee_value = assignees.iter().map(|u| u.username.as_str()).collect::<Vec<_>>().join(", ");
        body.push_str(&format!(
            "<div class=\"card\"><h3>Triage</h3>\
             <form method=\"post\" action=\"/{o}/{r}/issues/{number}/triage\">\
             <label>Labels</label>{label_boxes}\
             <input name=\"new_label\" placeholder=\"new label name\" aria-label=\"New label\" style=\"margin-top:4px\">\
             <label for=\"t-assignees\">Assignees</label>\
             <input id=\"t-assignees\" name=\"assignees\" value=\"{av}\" placeholder=\"alice, bob\">\
             <label for=\"t-ms\">Milestone</label>\
             <select id=\"t-ms\" name=\"milestone\">{ms_options}</select>\
             <p style=\"margin-top:10px\"><button class=\"btn\" type=\"submit\">Save triage</button></p>\
             </form></div>",
            av = esc(&assignee_value),
        ));
    }
    body.push_str("</div></div>");
    page(&format!("#{number} {}", issue.title), &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct CommentForm {
    body: String,
}

/// POST /{org}/{repo}/issues/{number}/comment — HTML fragment for htmx,
/// redirect otherwise.
pub async fn issue_comment(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<CommentForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/issues/{number}");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::comment_issue(&state, &r, number, &user, &form.body).await {
        Ok(comment) if is_htmx(&headers) => Html(comment_div(
            &user.username,
            comment.created_at.unix_timestamp(),
            &comment.body,
        ))
        .into_response(),
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

/// POST /{org}/{repo}/issues/{number}/state — close or reopen (author or Write).
pub async fn issue_state(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<StateForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/issues/{number}");
    let (meta, user, _) = match web_repo_participant(&state, &session, &org, &repo, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let issue = match state.store.get_issue(meta.id, number).await {
        Ok(i) => i,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "issue not found"),
    };
    if !svc::is_author_or_role(&state, &user, &meta, issue.author_id, RepoRole::Write).await {
        return error_page(&session, StatusCode::FORBIDDEN, "only the author or a writer can change this issue");
    }
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    match svc::set_issue_state(&state, &r, number, &user, IssueState::parse(&form.state)).await {
        Ok(_) => Redirect::to(&next).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/issues/{number}/triage — labels/assignees/milestone
/// (Write). The form posts repeated `label` fields, so decode manually.
pub async fn issue_triage(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/issues/{number}");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let pairs = parse_form_pairs(&body);
    let existing = state.store.list_labels(meta.id).await.unwrap_or_default();
    let mut labels: Vec<LabelSpec> = pairs
        .iter()
        .filter(|(k, _)| k == "label")
        .filter_map(|(_, v)| existing.iter().find(|l| &l.name == v))
        .map(|l| LabelSpec { name: l.name.clone(), color: Some(l.color.clone()) })
        .collect();
    if let Some((_, new_label)) = pairs.iter().find(|(k, v)| k == "new_label" && !v.trim().is_empty()) {
        labels.push(LabelSpec { name: new_label.trim().to_string(), color: None });
    }
    let assignees: Vec<String> = pairs
        .iter()
        .find(|(k, _)| k == "assignees")
        .map(|(_, v)| {
            v.split(',')
                .map(|s| s.trim().trim_start_matches('@').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let milestone = pairs
        .iter()
        .find(|(k, _)| k == "milestone")
        .and_then(|(_, v)| v.parse::<i32>().ok())
        .unwrap_or(0);
    let r = RepoRef { org: &org, repo: &repo, meta: &meta };
    let patch = IssuePatch {
        labels: Some(labels),
        assignees: Some(assignees),
        milestone: Some(milestone),
        ..IssuePatch::default()
    };
    match svc::update_issue(&state, &r, number, &user, patch).await {
        Ok(_) => Redirect::to(&format!("{next}?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// Decode an `application/x-www-form-urlencoded` body into ordered pairs
/// (supports repeated keys, which `Form<T>` cannot).
pub fn parse_form_pairs(body: &[u8]) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    match std::str::from_utf8(&bytes[i + 1..i + 3])
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                        .ok_or(())
                    {
                        Ok(b) => {
                            out.push(b);
                            i += 2;
                        }
                        Err(_) => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k), decode(v))
        })
        .collect()
}

// --- labels -----------------------------------------------------------------

/// GET /{org}/{repo}/labels
pub async fn labels_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<StateQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let labels = match state.store.list_labels(meta.id).await {
        Ok(l) => l,
        Err(e) => return app_error_page(&session, e.into()),
    };
    let can_write = role.is_some_and(|r| r.can_write());
    let (o, r) = (esc(&org), esc(&repo));
    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "issues", role));
    body.push_str("<div class=\"card\"><h3>Labels</h3>");
    if labels.is_empty() {
        body.push_str("<p class=\"muted\">No labels yet.</p>");
    } else if can_write {
        // Writers get an inline edit row (rename / recolor) and a delete action.
        for l in &labels {
            let id = l.id;
            body.push_str(&format!(
                "<div class=\"row\" style=\"margin:6px 0\">{chip}\
                 <form method=\"post\" action=\"/{o}/{r}/labels/{id}/edit\" class=\"row\" style=\"flex:1\">\
                 <input name=\"name\" value=\"{name}\" required maxlength=\"50\" style=\"flex:2\" aria-label=\"Label name\">\
                 <input name=\"color\" type=\"color\" value=\"#{color}\" style=\"width:60px;padding:2px\" aria-label=\"Label color\">\
                 <button class=\"btn small\" type=\"submit\">Save</button></form>\
                 <form method=\"post\" action=\"/{o}/{r}/labels/{id}/delete\" \
                 onsubmit=\"return confirm('Delete label {name}? It will be removed from every issue.')\">\
                 <button class=\"btn small danger\" type=\"submit\">Delete</button></form></div>",
                chip = label_chip(&l.name, &l.color),
                name = esc(&l.name),
                color = esc(&l.color),
            ));
        }
    } else {
        body.push_str("<div class=\"row\">");
        for l in &labels {
            body.push_str(&label_chip(&l.name, &l.color));
        }
        body.push_str("</div>");
    }
    if can_write {
        body.push_str(&format!(
            "<form method=\"post\" action=\"/{o}/{r}/labels\" class=\"row\" style=\"margin-top:12px\">\
             <input name=\"name\" placeholder=\"label name\" required maxlength=\"50\" style=\"flex:2\" aria-label=\"Label name\">\
             <input name=\"color\" type=\"color\" value=\"#0969da\" style=\"width:60px;padding:2px\" aria-label=\"Label color\">\
             <button class=\"btn\" type=\"submit\">Create label</button></form>"
        ));
    }
    body.push_str("</div>");
    page("Labels", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct LabelForm {
    name: String,
    #[serde(default)]
    color: String,
}

/// POST /{org}/{repo}/labels (Write)
pub async fn label_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<LabelForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/labels");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match crate::collab_svc::create_label(&state, &meta, &user, &form.name, Some(&form.color)).await {
        Ok(_) => Redirect::to(&format!("{next}?notice=created")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/labels/{id}/edit (Write)
pub async fn label_edit(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, uuid::Uuid)>,
    headers: HeaderMap,
    Form(form): Form<LabelForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/labels");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let color = Some(form.color.as_str()).filter(|c| !c.trim().is_empty());
    match crate::collab_svc::edit_label(&state, &meta, &user, id, Some(&form.name), color).await {
        Ok(_) => Redirect::to(&format!("{next}?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/labels/{id}/delete (Write)
pub async fn label_delete(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, uuid::Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/labels");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match crate::collab_svc::delete_label(&state, &meta, &user, id).await {
        Ok(()) => Redirect::to(&format!("{next}?notice=deleted")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

// --- milestones -------------------------------------------------------------

/// GET /{org}/{repo}/milestones
pub async fn milestones_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<StateQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let can_write = role.is_some_and(|r| r.can_write());
    let milestones = state.store.list_milestones(meta.id).await.unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "milestones", role));
    body.push_str("<div class=\"card\"><h3>Milestones</h3>");
    if milestones.is_empty() {
        body.push_str("<p class=\"muted\">No milestones yet. Milestones group issues toward a goal or release.</p>");
    }
    for m in &milestones {
        let total = m.open_issues + m.closed_issues;
        let pct = if total > 0 { m.closed_issues * 100 / total } else { 0 };
        let closed = m.milestone.closed_at.is_some();
        let due = m
            .milestone
            .due_on
            .map(|d| format!(" \u{00b7} due {}", short_date(d.unix_timestamp())))
            .unwrap_or_default();
        let controls = if can_write {
            let (label, target) = if closed { ("Reopen", "open") } else { ("Close", "closed") };
            format!(
                "<form method=\"post\" action=\"/{o}/{r}/milestones/{n}/state\" class=\"inline-form\">\
                 <input type=\"hidden\" name=\"state\" value=\"{target}\">\
                 <button class=\"btn small\" type=\"submit\">{label}</button></form> \
                 <form method=\"post\" action=\"/{o}/{r}/milestones/{n}/delete\" class=\"inline-form\">\
                 <button class=\"btn small danger\" type=\"submit\">Delete</button></form>",
                n = m.milestone.number
            )
        } else {
            String::new()
        };
        body.push_str(&format!(
            "<div class=\"comment\"><div class=\"comment-h row\"><strong>{t}</strong> {pill}\
             <span class=\"muted\">{pct}% complete \u{00b7} {open} open \u{00b7} {cl} closed{due}</span>\
             <span class=\"spacer\"></span>{controls}</div>\
             <div class=\"comment-b\"><progress max=\"100\" value=\"{pct}\" style=\"width:100%\"></progress>{desc}</div></div>",
            t = esc(&m.milestone.title),
            pill = state_pill(!closed),
            open = m.open_issues,
            cl = m.closed_issues,
            desc = m
                .milestone
                .description
                .as_deref()
                .map(|d| format!("<div class=\"readme\">{}</div>", markdown(d)))
                .unwrap_or_default(),
        ));
    }
    body.push_str("</div>");
    if can_write {
        body.push_str(&format!(
            "<div class=\"card\"><h3>New milestone</h3>\
             <form method=\"post\" action=\"/{o}/{r}/milestones\">\
             <label for=\"m-title\">Title</label><input id=\"m-title\" name=\"title\" required maxlength=\"200\">\
             <label for=\"m-due\">Due date <span class=\"muted\">(optional)</span></label>\
             <input id=\"m-due\" name=\"due_on\" type=\"date\">\
             <label for=\"m-desc\">Description</label><textarea id=\"m-desc\" name=\"description\" rows=\"3\"></textarea>\
             <p style=\"margin-top:10px\"><button class=\"btn primary\" type=\"submit\">Create milestone</button></p>\
             </form></div>"
        ));
    }
    page("Milestones", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct MilestoneForm {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    due_on: String,
}

fn parse_due(date: &str) -> Result<Option<time::OffsetDateTime>, String> {
    let d = date.trim();
    if d.is_empty() {
        return Ok(None);
    }
    let err = || "due date must be YYYY-MM-DD".to_string();
    let mut parts = d.splitn(3, '-');
    let year: i32 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(err)?;
    let month: u8 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(err)?;
    let day: u8 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(err)?;
    let month = time::Month::try_from(month).map_err(|_| err())?;
    let date = time::Date::from_calendar_date(year, month, day).map_err(|_| err())?;
    let end_of_day = date.with_hms(23, 59, 59).map_err(|_| err())?;
    Ok(Some(end_of_day.assume_utc()))
}

/// POST /{org}/{repo}/milestones (Write)
pub async fn milestone_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<MilestoneForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/milestones");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if form.title.trim().is_empty() {
        return error_page(&session, StatusCode::BAD_REQUEST, "milestone title is required");
    }
    let due_on = match parse_due(&form.due_on) {
        Ok(d) => d,
        Err(msg) => return error_page(&session, StatusCode::BAD_REQUEST, &msg),
    };
    let input = NewMilestone {
        title: form.title.trim().to_string(),
        description: Some(form.description.trim().to_string()).filter(|d| !d.is_empty()),
        due_on,
    };
    match state.store.create_milestone(meta.id, &input).await {
        Ok(_) => Redirect::to(&format!("{next}?notice=created")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

/// POST /{org}/{repo}/milestones/{number}/state (Write)
pub async fn milestone_state(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Form(form): Form<StateForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/milestones");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let patch = UpdateMilestone {
        title: None,
        description: None,
        due_on: None,
        state: Some(if form.state == "closed" { "closed".into() } else { "open".into() }),
    };
    match state.store.update_milestone(meta.id, number, &patch).await {
        Ok(_) => Redirect::to(&format!("{next}?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

/// POST /{org}/{repo}/milestones/{number}/delete (Write)
pub async fn milestone_delete(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/milestones");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match state.store.delete_milestone(meta.id, number).await {
        Ok(()) => Redirect::to(&format!("{next}?notice=deleted")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

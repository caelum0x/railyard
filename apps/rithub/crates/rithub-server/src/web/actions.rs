//! Actions: CI check runs for any branch, a per-run detail page (status,
//! timing, output summary, details link), and runner management (register a
//! runner and receive its one-time token; delete runners).
//!
//! Reads follow repository visibility; the runner list needs Write and runner
//! registration/deletion needs Admin (matching the JSON API).

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{CiStore, RepoRole};
use serde::Deserialize;
use uuid::Uuid;

use super::{
    app_error_page, check_runs_html, error_page, esc, markdown, notice_html, page, public_base,
    repo_header, session_user, short_date, web_repo_read, web_repo_role,
};
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct ActionsQuery {
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    notice: Option<String>,
}

fn datetime(t: Option<time::OffsetDateTime>) -> String {
    t.map(|t| {
        format!(
            "{} {:02}:{:02} UTC",
            t.date(),
            t.hour(),
            t.minute()
        )
    })
    .unwrap_or_else(|| "\u{2014}".into())
}

async fn runners_card(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
    repo_id: Uuid,
    role: Option<RepoRole>,
    fresh_token: Option<&str>,
) -> String {
    if !role.is_some_and(|r| r.can_write()) {
        return String::new();
    }
    let is_admin = role.is_some_and(|r| r.can_admin());
    let runners = state.store.list_runners(repo_id).await.unwrap_or_default();
    let (o, r) = (esc(org), esc(repo));
    let api = format!("{}/api/orgs/{org}/repos/{repo}", public_base(headers));
    let mut out = String::from("<div class=\"card\"><h3>Runners</h3>");
    if let Some(token) = fresh_token {
        out.push_str(&format!(
            "<div class=\"notice ok-notice\"><strong>Runner registered.</strong> Configure it with this token \
             (shown once):<div class=\"secret mono\" style=\"margin-top:6px\">{t}</div>\
             <p class=\"hint\">The runner reports with <code>Authorization: Bearer &lt;token&gt;</code> to \
             <code>POST {api}/check-runs</code> and <code>PATCH {api}/check-runs/&lt;id&gt;</code>. \
             Subscribe a <a href=\"/{o}/{r}/settings/hooks\">webhook</a> to the <code>check_run</code> event to receive work.</p></div>",
            t = esc(token),
            api = esc(&api),
        ));
    }
    if runners.is_empty() {
        out.push_str("<p class=\"muted\">No runners registered.</p>");
    } else {
        out.push_str("<table><tr><th>Name</th><th>Last seen</th><th>Registered</th><th></th></tr>");
        for runner in &runners {
            let delete = if is_admin {
                format!(
                    "<form method=\"post\" action=\"/{o}/{r}/actions/runners/{id}/delete\" class=\"inline-form\">\
                     <button class=\"btn small danger\" type=\"submit\">Remove</button></form>",
                    id = runner.id
                )
            } else {
                String::new()
            };
            out.push_str(&format!(
                "<tr><td><strong>{n}</strong></td><td class=\"muted\">{seen}</td><td class=\"muted\">{reg}</td><td>{delete}</td></tr>",
                n = esc(&runner.name),
                seen = runner
                    .last_seen_at
                    .map(|t| short_date(t.unix_timestamp()))
                    .unwrap_or_else(|| "never".into()),
                reg = short_date(runner.created_at.unix_timestamp()),
            ));
        }
        out.push_str("</table>");
    }
    if is_admin {
        out.push_str(&format!(
            "<form method=\"post\" action=\"/{o}/{r}/actions/runners\" class=\"row\" style=\"margin-top:10px\">\
             <input name=\"name\" placeholder=\"runner name\" required maxlength=\"100\" style=\"flex:1\" aria-label=\"Runner name\">\
             <button class=\"btn\" type=\"submit\">Register runner</button></form>"
        ));
    }
    out.push_str("</div>");
    out
}

/// GET /{org}/{repo}/actions?branch=
pub async fn actions_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<ActionsQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let branches = state.host.branches(&org, &repo).unwrap_or_default();
    let default_branch = state.host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());
    let branch = q
        .branch
        .clone()
        .filter(|b| branches.contains(b))
        .unwrap_or(default_branch);
    let head = state.host.resolve_branch(&org, &repo, &branch).ok().map(|oid| oid.to_hex());

    let (o, r) = (esc(&org), esc(&repo));
    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "actions", role));
    let options: String = branches
        .iter()
        .map(|b| {
            let sel = if *b == branch { " selected" } else { "" };
            format!("<option value=\"{v}\"{sel}>{v}</option>", v = esc(b))
        })
        .collect();
    body.push_str(&format!(
        "<div class=\"card\"><form method=\"get\" action=\"/{o}/{r}/actions\" class=\"row\">\
         <label for=\"a-branch\" style=\"margin:0\">Branch</label>\
         <select id=\"a-branch\" name=\"branch\" style=\"width:auto\" onchange=\"this.form.submit()\">{options}</select>\
         <noscript><button class=\"btn small\" type=\"submit\">Show</button></noscript></form>"
    ));
    match &head {
        Some(sha) => {
            let runs = state.store.latest_check_runs(meta.id, sha).await.unwrap_or_default();
            body.push_str(&format!(
                "<h3>Check runs on <code>{b}</code> <span class=\"muted mono\">{short}</span></h3>",
                b = esc(&branch),
                short = esc(&sha[..sha.len().min(8)])
            ));
            body.push_str(&check_runs_html(&org, &repo, &runs));
        }
        None => body.push_str("<p class=\"muted\">No commits yet. Check runs appear after the first push.</p>"),
    }
    body.push_str("</div>");
    body.push_str(&runners_card(&state, &headers, &org, &repo, meta.id, role, None).await);
    page("Actions", &session, &body).into_response()
}

/// GET /{org}/{repo}/actions/runs/{id}
pub async fn run_detail(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let run = match state.store.get_check_run(meta.id, id).await {
        Ok(run) => run,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "check run not found"),
    };
    let (o, r) = (esc(&org), esc(&repo));
    let conclusion = run.conclusion.map(|c| c.as_str()).unwrap_or("\u{2014}");
    let details = run
        .details_url
        .as_deref()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(|u| format!("<a class=\"btn small\" href=\"{u}\" rel=\"noopener noreferrer\" target=\"_blank\">Open full log</a>", u = esc(u)))
        .unwrap_or_default();
    let mut body = repo_header(&org, &repo, &meta, "actions", role);
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row\"><h2>{name}</h2><span class=\"pill\">{status}</span>\
         <span class=\"spacer\"></span>{details}</div>\
         <table><tr><td>Commit</td><td><a class=\"mono\" href=\"/{o}/{r}/commit/{sha}\">{sha}</a></td></tr>\
         <tr><td>Conclusion</td><td>{conclusion}</td></tr>\
         <tr><td>Queued</td><td>{created}</td></tr>\
         <tr><td>Started</td><td>{started}</td></tr>\
         <tr><td>Completed</td><td>{completed}</td></tr></table></div>",
        name = esc(&run.name),
        status = esc(run.status.as_str()),
        sha = esc(&run.head_sha),
        conclusion = esc(conclusion),
        created = datetime(Some(run.created_at)),
        started = datetime(run.started_at),
        completed = datetime(run.completed_at),
    ));
    body.push_str(&format!(
        "<div class=\"card\"><h3>{title}</h3><div class=\"readme\">{summary}</div></div>",
        title = esc(run.output_title.as_deref().unwrap_or("Output")),
        summary = run
            .output_summary
            .as_deref()
            .map(markdown)
            .unwrap_or_else(|| "<p class=\"muted\">The runner has not reported output yet.</p>".into()),
    ));
    page(&run.name, &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct RunnerForm {
    name: String,
}

/// POST /{org}/{repo}/actions/runners (Admin) — shows the one-time token.
pub async fn runner_register(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<RunnerForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/actions");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Admin, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match crate::ci::add_runner(&state, &meta, &user, &form.name).await {
        Ok(created) => {
            let mut body = repo_header(&org, &repo, &meta, "actions", Some(RepoRole::Admin));
            body.push_str(
                &runners_card(&state, &headers, &org, &repo, meta.id, Some(RepoRole::Admin), Some(&created.token))
                    .await,
            );
            page("Runner registered", &session, &body).into_response()
        }
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/actions/runners/{id}/delete (Admin)
pub async fn runner_delete(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/actions");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Admin, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match state.store.delete_runner(meta.id, id).await {
        Ok(()) => {
            crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "runner.deleted", Some(&id.to_string()), None)
                .await;
            Redirect::to(&format!("{next}?notice=removed")).into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

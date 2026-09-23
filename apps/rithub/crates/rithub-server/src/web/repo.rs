//! Repository browse pages: home (file tree + README), tree navigation, blob
//! view, commit history, and a single-commit diff. All content is read live from
//! `rit` via `RepoHost`.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rithub_core::{Repository as RepoMeta, Store};
use rithub_git::{DiffResult, RepoHost};

use super::{error_page, esc, markdown, page, session_user, short_date};
use crate::state::AppState;

/// Look up repo metadata or render a 404 page.
async fn repo_or_404(
    state: &AppState,
    session: &Option<rithub_core::User>,
    org: &str,
    repo: &str,
) -> Result<RepoMeta, Response> {
    state
        .store
        .get_repo(org, repo)
        .await
        .map_err(|_| error_page(session, StatusCode::NOT_FOUND, "repository not found"))
}

/// The repo tab bar (Code / Issues / Pull requests / Releases).
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

/// A branch selector that navigates the tree view on change.
fn branch_select(org: &str, repo: &str, branches: &[String], current: &str) -> String {
    if branches.is_empty() {
        return String::new();
    }
    let mut opts = String::new();
    for b in branches {
        let sel = if b == current { " selected" } else { "" };
        opts.push_str(&format!(
            "<option value=\"/{org}/{repo}/tree/{b}\"{sel}>{b}</option>",
            b = esc(b)
        ));
    }
    format!(
        "<select style=\"width:auto\" onchange=\"location=this.value\">{opts}</select>"
    )
}

/// The `git clone` URL box, using the request Host header for a real URL.
fn clone_box(headers: &HeaderMap, org: &str, repo: &str) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!(
        "<div class=\"card\"><strong>Clone</strong><pre>git clone http://{host}/{org}/{repo}.git</pre></div>",
        org = esc(org),
        repo = esc(repo)
    )
}

/// GET /{org}/{repo}
pub async fn home(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let host = &state.host;
    let default_branch = host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());

    let mut body = format!("<div class=\"card\"><h1>{}/{}</h1>", esc(&org), esc(&repo));
    body.push_str(&tabs(&org, &repo, "code"));

    match host.is_empty_repo(&org, &repo) {
        Ok(true) | Err(_) => {
            body.push_str("<p class=\"muted\">This repository is empty. Push a commit to get started.</p></div>");
            body.push_str(&clone_box(&headers, &org, &repo));
            return page(&format!("{org}/{repo}"), &session, &body).into_response();
        }
        Ok(false) => {}
    }

    let branches = host.branches(&org, &repo).unwrap_or_default();
    body.push_str(&format!(
        "<div class=\"row\">{} <a href=\"/{org}/{repo}/commits/{b}\">commits</a></div>",
        branch_select(&org, &repo, &branches, &default_branch),
        b = esc(&default_branch)
    ));
    body.push_str("</div>");

    // File tree at HEAD of the default branch.
    let commit = match host.resolve_commit(&org, &repo, &default_branch) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "branch not found"),
    };
    body.push_str(&render_tree(host, &org, &repo, &default_branch, "", commit));

    // Rendered README, if present at the root.
    if let Ok(content) = host.read_blob_at(&org, &repo, commit, "README.md") {
        if !content.binary {
            let md = String::from_utf8_lossy(&content.bytes);
            body.push_str(&format!(
                "<div class=\"card\"><strong>README.md</strong><div class=\"readme\">{}</div></div>",
                markdown(&md)
            ));
        }
    }

    body.push_str(&clone_box(&headers, &org, &repo));
    page(&format!("{org}/{repo}"), &session, &body).into_response()
}

/// Render a tree listing card for `path` at `rev`.
fn render_tree(
    host: &RepoHost,
    org: &str,
    repo: &str,
    rev: &str,
    path: &str,
    commit: rithub_git::ObjectId,
) -> String {
    let items = match host.list_tree(org, repo, commit, path) {
        Ok(items) => items,
        Err(_) => return "<div class=\"card\"><p class=\"bad\">path not found</p></div>".into(),
    };
    let mut out = String::from("<div class=\"card\"><table>");
    if !path.is_empty() {
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let href = if parent.is_empty() {
            format!("/{org}/{repo}/tree/{rev}")
        } else {
            format!("/{org}/{repo}/tree/{rev}/{parent}")
        };
        out.push_str(&format!("<tr><td><a href=\"{href}\">..</a></td></tr>"));
    }
    for item in items {
        let child = if path.is_empty() {
            item.name.clone()
        } else {
            format!("{path}/{}", item.name)
        };
        let (icon, href) = if item.is_dir {
            ("📁", format!("/{org}/{repo}/tree/{rev}/{}", esc(&child)))
        } else {
            ("📄", format!("/{org}/{repo}/blob/{rev}/{}", esc(&child)))
        };
        out.push_str(&format!(
            "<tr><td>{icon} <a href=\"{href}\">{}</a></td></tr>",
            esc(&item.name)
        ));
    }
    out.push_str("</table></div>");
    out
}

/// GET /{org}/{repo}/tree/{rev}
pub async fn tree_root(
    State(state): State<AppState>,
    Path((org, repo, rev)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    tree_common(state, headers, org, repo, rev, String::new()).await
}

/// GET /{org}/{repo}/tree/{rev}/{*path}
pub async fn tree_path(
    State(state): State<AppState>,
    Path((org, repo, rev, path)): Path<(String, String, String, String)>,
    headers: HeaderMap,
) -> Response {
    tree_common(state, headers, org, repo, rev, path).await
}

async fn tree_common(
    state: AppState,
    headers: HeaderMap,
    org: String,
    repo: String,
    rev: String,
    path: String,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}<p class=\"muted\">{} @ {}</p></div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "code"),
        esc(&rev),
        esc(if path.is_empty() { "/" } else { &path })
    );
    body.push_str(&render_tree(&state.host, &org, &repo, &rev, &path, commit));
    page(&format!("{org}/{repo}"), &session, &body).into_response()
}

/// GET /{org}/{repo}/blob/{rev}/{*path}
pub async fn blob(
    State(state): State<AppState>,
    Path((org, repo, rev, path)): Path<(String, String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let content = match state.host.read_blob_at(&org, &repo, commit, &path) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "file not found"),
    };
    let mut body = format!(
        "<div class=\"card\"><h2>{}</h2><p class=\"muted\">{} @ {}</p></div>",
        esc(&path),
        esc(&org),
        esc(&rev)
    );
    if content.binary {
        body.push_str(&format!(
            "<div class=\"card\"><p class=\"muted\">Binary file ({} bytes) not shown.</p></div>",
            content.bytes.len()
        ));
    } else {
        let text = String::from_utf8_lossy(&content.bytes);
        // Syntax-highlight to class-based spans; fall back to escaped plain text
        // for binary-ish, oversized, or unrecognized files.
        match forge_search::highlight::highlight_blob_html(&text, &path) {
            Some(html) => {
                body.push_str(&format!("<pre class=\"hlcode\">{html}</pre>"));
            }
            None => {
                body.push_str(&format!("<pre>{}</pre>", esc(&text)));
            }
        }
    }
    page(&esc(&path), &session, &body).into_response()
}

/// GET /{org}/{repo}/commits/{rev}
pub async fn commits(
    State(state): State<AppState>,
    Path((org, repo, rev)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let history = state.host.history(&org, &repo, commit, 100).unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}<h3>Commits on {}</h3></div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo, "code"),
        esc(&rev)
    );
    body.push_str("<div class=\"card\"><table>");
    for c in history {
        body.push_str(&format!(
            "<tr><td><a href=\"/{org}/{repo}/commit/{oid}\">{summary}</a><br>\
<span class=\"muted\">{short} · {who} · {date}</span></td></tr>",
            org = esc(&org),
            repo = esc(&repo),
            oid = esc(&c.oid),
            summary = esc(&c.summary),
            short = esc(&c.oid[..c.oid.len().min(8)]),
            who = esc(&c.author_name),
            date = short_date(c.time)
        ));
    }
    body.push_str("</table></div>");
    page("Commits", &session, &body).into_response()
}

/// GET /{org}/{repo}/commit/{oid}
pub async fn commit_detail(
    State(state): State<AppState>,
    Path((org, repo, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let commit = match state.host.resolve_commit(&org, &repo, &oid) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "commit not found"),
    };
    let (info, diff) = match state.host.commit_detail(&org, &repo, commit) {
        Ok(v) => v,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "diff failed"),
    };
    let mut body = format!(
        "<div class=\"card\"><h2>{}</h2><p class=\"muted\">{} · {} · {}</p><pre>{}</pre></div>",
        esc(&info.summary),
        esc(&info.oid),
        esc(&info.author_name),
        short_date(info.time),
        esc(&info.message)
    );
    body.push_str(&render_diff_html(&diff));
    page("Commit", &session, &body).into_response()
}

/// Render a `DiffResult` as coloured unified-diff HTML.
pub fn render_diff_html(diff: &DiffResult) -> String {
    let mut out = format!(
        "<div class=\"card\"><strong>{} file(s) changed</strong> <span class=\"ok\">+{}</span> <span class=\"bad\">-{}</span></div>",
        diff.files.len(),
        diff.additions,
        diff.deletions
    );
    for file in &diff.files {
        out.push_str(&format!(
            "<div class=\"diff\"><div class=\"h\">{} {} <span class=\"ok\">+{}</span> <span class=\"bad\">-{}</span></div>",
            file.status,
            esc(&file.path),
            file.additions,
            file.deletions
        ));
        if file.binary {
            out.push_str("<pre>Binary file not shown.</pre></div>");
            continue;
        }
        // Syntax-highlighted unified diff: each line carries its diff role class
        // (add/del/ctx/hunk) plus per-line language spans. All content is escaped
        // inside the highlighter, so the patch text cannot inject markup.
        out.push_str("<pre class=\"hlcode hldiff\">");
        out.push_str(&forge_search::highlight::highlight_diff_html(
            &file.path,
            &file.patch,
        ));
        out.push_str("</pre></div>");
    }
    out
}

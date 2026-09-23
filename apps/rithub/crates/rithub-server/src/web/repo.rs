//! Repository browse pages: home (file tree + README + clone URLs), tree
//! navigation, blob view, raw download, commit history, a single-commit diff,
//! a source-archive download for any revision, and the "Fork" action. All
//! content is read live from `rit` via `RepoHost`; every page is gated by
//! [`super::web_repo_read`] so private repositories are invisible without access.

use axum::body::Body;
use axum::extract::{Form, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{CiStore, DirectoryStore, Role, Store};
use rithub_git::{DiffResult, RepoHost};
use serde::Deserialize;

use super::{
    app_error_page, breadcrumbs, check_runs_html, error_page, esc, icon, login_redirect,
    markdown, page, public_base, repo_header, session_user, short_date, web_repo_read,
};
use crate::state::AppState;

/// Max blob size rendered inline (larger files offer the raw download only).
const MAX_INLINE_BLOB: usize = 1024 * 1024;

/// The SSH clone host (`RITHUB_SSH_PUBLIC`, else derived from `RITHUB_SSH_BIND`).
pub fn ssh_host() -> Option<String> {
    if let Ok(public) = std::env::var("RITHUB_SSH_PUBLIC") {
        if !public.trim().is_empty() {
            return Some(public.trim().to_string());
        }
    }
    let bind = std::env::var("RITHUB_SSH_BIND").ok()?;
    let port = bind.rsplit_once(':').map(|(_, p)| p).unwrap_or("22");
    let host = std::env::var("RITHUB_PUBLIC_URL")
        .ok()
        .and_then(|u| {
            u.split_once("://")
                .map(|(_, rest)| rest.split(['/', ':']).next().unwrap_or("").to_string())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string());
    Some(if port == "22" { host } else { format!("{host}:{port}") })
}

/// The clone-URL card (HTTP(S) from the public base URL, plus SSH when enabled).
fn clone_box(headers: &HeaderMap, org: &str, repo: &str) -> String {
    let http = format!("{}/{org}/{repo}.git", public_base(headers));
    let ssh = ssh_host()
        .map(|h| {
            format!(
                "<label>SSH</label><pre>git clone ssh://git@{h}/{o}/{r}.git</pre>",
                h = esc(&h),
                o = esc(org),
                r = esc(repo)
            )
        })
        .unwrap_or_default();
    format!(
        "<div class=\"card\"><strong>Clone</strong>\
         <label>HTTPS</label><pre>git clone {http}</pre>{ssh}\
         <p class=\"hint\">Private repositories and pushes authenticate with your username and a \
         <a href=\"/settings/tokens\">personal access token</a>, or an <a href=\"/settings/keys\">SSH key</a>.</p></div>",
        http = esc(&http),
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
            "<option value=\"/{o}/{r}/tree/{b}\"{sel}>{b}</option>",
            o = esc(org),
            r = esc(repo),
            b = esc(b)
        ));
    }
    format!(
        "<select style=\"width:auto\" aria-label=\"Branch\" onchange=\"location.href=this.value\">{opts}</select>"
    )
}

/// Fork form: shown to signed-in users who can create repos somewhere.
async fn fork_control(state: &AppState, session: &Option<rithub_core::User>, org: &str, repo: &str) -> String {
    let Some(user) = session else {
        return String::new();
    };
    let targets: Vec<String> = if crate::authz::is_site_admin(user) {
        state.store.list_orgs().await.unwrap_or_default().into_iter().map(|o| o.name).collect()
    } else {
        state
            .store
            .list_user_orgs(user.id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(o, r)| r.can_manage_repos() && o.name != org)
            .map(|(o, _)| o.name)
            .collect()
    };
    let targets: Vec<String> = targets.into_iter().filter(|o| o != org).collect();
    if targets.is_empty() {
        return String::new();
    }
    let options: String = targets
        .iter()
        .map(|o| format!("<option value=\"{o}\">{o}</option>", o = esc(o)))
        .collect();
    format!(
        "<form method=\"post\" action=\"/{o}/{r}/fork\" class=\"row\" style=\"gap:4px\">\
         <select name=\"org\" style=\"width:auto\" aria-label=\"Fork into\">{options}</select>\
         <button class=\"btn\" type=\"submit\">{icon} Fork</button></form>",
        o = esc(org),
        r = esc(repo),
        icon = icon("fork"),
    )
}

/// GET /{org}/{repo}
pub async fn home(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let host = &state.host;
    let default_branch = host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());

    let mut body = repo_header(&org, &repo, &meta, "code", role);
    if let Ok(Some((po, pr))) = state.store.fork_parent(meta.id).await {
        body.push_str(&format!(
            "<p class=\"muted\">{icon} Forked from <a href=\"/{po}/{pr}\">{po}/{pr}</a></p>",
            icon = icon("fork"),
            po = esc(&po),
            pr = esc(&pr)
        ));
    }

    if matches!(host.is_empty_repo(&org, &repo), Ok(true) | Err(_)) {
        let push_hint = if role.is_some_and(|r| r.can_write()) {
            format!(
                "<pre>git remote add origin {base}/{o}/{r}.git\ngit push -u origin {b}</pre>",
                base = esc(&public_base(&headers)),
                o = esc(&org),
                r = esc(&repo),
                b = esc(&default_branch)
            )
        } else {
            String::new()
        };
        body.push_str(&format!(
            "<div class=\"card\"><div class=\"empty-state\"><h3>This repository is empty</h3>\
             <p>Push a commit to get started.</p></div>{push_hint}</div>"
        ));
        body.push_str(&clone_box(&headers, &org, &repo));
        return page(&format!("{org}/{repo}"), &session, &body).into_response();
    }

    let branches = host.branches(&org, &repo).unwrap_or_default();
    let history = host
        .resolve_commit(&org, &repo, &default_branch)
        .ok()
        .and_then(|c| host.history(&org, &repo, c, 1).ok())
        .and_then(|h| h.into_iter().next());
    let last = history
        .map(|c| {
            format!(
                "<span class=\"muted\"><a href=\"/{o}/{r}/commit/{oid}\">{s}</a> \u{00b7} {who} \u{00b7} {d}</span>",
                o = esc(&org),
                r = esc(&repo),
                oid = esc(&c.oid),
                s = esc(&c.summary),
                who = esc(&c.author_name),
                d = short_date(c.time)
            )
        })
        .unwrap_or_default();
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row\">{sel}\
         <a href=\"/{o}/{r}/commits/{b}\">History</a>\
         <a href=\"/{o}/{r}/archive/{b}.tar.gz\" hx-boost=\"false\">Download tar.gz</a>\
         <span class=\"spacer\"></span>{fork}</div><p>{last}</p></div>",
        sel = branch_select(&org, &repo, &branches, &default_branch),
        o = esc(&org),
        r = esc(&repo),
        b = esc(&default_branch),
        fork = fork_control(&state, &session, &org, &repo).await,
    ));

    let commit = match host.resolve_commit(&org, &repo, &default_branch) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "branch not found"),
    };
    body.push_str(&render_tree(host, &org, &repo, &default_branch, "", commit));

    for readme in ["README.md", "README", "readme.md", "README.txt"] {
        if let Ok(content) = host.read_blob_at(&org, &repo, commit, readme) {
            if !content.binary {
                let text = String::from_utf8_lossy(&content.bytes);
                let rendered = if readme.ends_with(".md") {
                    markdown(&text)
                } else {
                    format!("<pre>{}</pre>", esc(&text))
                };
                body.push_str(&format!(
                    "<div class=\"card\"><strong>{name}</strong><div class=\"readme\">{rendered}</div></div>",
                    name = esc(readme)
                ));
            }
            break;
        }
    }

    body.push_str(&clone_box(&headers, &org, &repo));
    page(&format!("{org}/{repo}"), &session, &body).into_response()
}

/// Render a tree listing card for `path` at `rev`.
fn render_tree(host: &RepoHost, org: &str, repo: &str, rev: &str, path: &str, commit: rithub_git::ObjectId) -> String {
    let items = match host.list_tree(org, repo, commit, path) {
        Ok(items) => items,
        Err(_) => return "<div class=\"card\"><p class=\"bad\">Path not found at this revision.</p></div>".into(),
    };
    let (o, r, rv) = (esc(org), esc(repo), esc(rev));
    let mut out = String::from("<div class=\"card\" id=\"file-tree\"><table>");
    if !path.is_empty() {
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let href = if parent.is_empty() {
            format!("/{o}/{r}/tree/{rv}")
        } else {
            format!("/{o}/{r}/tree/{rv}/{}", esc(parent))
        };
        out.push_str(&format!(
            "<tr><td><a href=\"{href}\" hx-get=\"{href}\" hx-target=\"#file-tree\" hx-select=\"#file-tree\" \
             hx-swap=\"outerHTML\" hx-push-url=\"true\">..</a></td></tr>"
        ));
    }
    if items.is_empty() {
        out.push_str("<tr><td class=\"muted\">Empty directory.</td></tr>");
    }
    let mut sorted = items;
    sorted.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    for item in sorted {
        let child = if path.is_empty() { item.name.clone() } else { format!("{path}/{}", item.name) };
        let (glyph, href, htmx_attrs) = if item.is_dir {
            let h = format!("/{o}/{r}/tree/{rv}/{}", esc(&child));
            let attrs = format!(
                " hx-get=\"{h}\" hx-target=\"#file-tree\" hx-select=\"#file-tree\" hx-swap=\"outerHTML\" hx-push-url=\"true\""
            );
            (icon("folder"), h, attrs)
        } else {
            (icon("file"), format!("/{o}/{r}/blob/{rv}/{}", esc(&child)), String::new())
        };
        out.push_str(&format!(
            "<tr><td><a href=\"{href}\"{htmx_attrs}>{glyph} {name}</a></td></tr>",
            name = esc(&item.name),
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

fn path_crumbs(org: &str, repo: &str, rev: &str, path: &str) -> String {
    let home = format!("/{org}/{repo}");
    let root = format!("/{org}/{repo}/tree/{rev}");
    let mut hrefs: Vec<(String, Option<String>)> = vec![
        (format!("{org}/{repo}"), Some(home)),
        (rev.to_string(), Some(root)),
    ];
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut acc = String::new();
    for (i, seg) in segs.iter().enumerate() {
        if i > 0 {
            acc.push('/');
        }
        acc.push_str(seg);
        let href = (i + 1 < segs.len()).then(|| format!("/{org}/{repo}/tree/{rev}/{acc}"));
        hrefs.push((seg.to_string(), href));
    }
    let parts: Vec<(&str, Option<&str>)> =
        hrefs.iter().map(|(l, h)| (l.as_str(), h.as_deref())).collect();
    breadcrumbs(&parts)
}

async fn tree_common(state: AppState, headers: HeaderMap, org: String, repo: String, rev: String, path: String) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let branches = state.host.branches(&org, &repo).unwrap_or_default();
    let mut body = repo_header(&org, &repo, &meta, "code", role);
    body.push_str(&format!(
        "<div class=\"row\" style=\"margin-bottom:8px\">{}{}</div>",
        branch_select(&org, &repo, &branches, &rev),
        path_crumbs(&org, &repo, &rev, &path)
    ));
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
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let content = match state.host.read_blob_at(&org, &repo, commit, &path) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "file not found"),
    };
    let raw_href = format!("/{}/{}/raw/{}/{}", esc(&org), esc(&repo), esc(&rev), esc(&path));
    let mut body = repo_header(&org, &repo, &meta, "code", role);
    body.push_str(&path_crumbs(&org, &repo, &rev, &path));
    body.push_str(&format!(
        "<div class=\"card\"><div class=\"row\"><strong>{name}</strong>\
         <span class=\"muted\">{size} bytes</span><span class=\"spacer\"></span>\
         <a class=\"btn small\" href=\"{raw_href}\" hx-boost=\"false\">Raw</a>\
         <a class=\"btn small\" href=\"{raw_href}?download=1\" hx-boost=\"false\">Download</a>\
         <a class=\"btn small\" href=\"/{o}/{r}/commits/{rv}\">History</a></div></div>",
        name = esc(path.rsplit('/').next().unwrap_or(&path)),
        size = content.bytes.len(),
        o = esc(&org),
        r = esc(&repo),
        rv = esc(&rev),
    ));
    if content.binary {
        body.push_str("<div class=\"card\"><p class=\"muted\">Binary file not shown. Use Raw or Download.</p></div>");
    } else if content.bytes.len() > MAX_INLINE_BLOB {
        body.push_str("<div class=\"card\"><p class=\"muted\">File too large to display. Use Raw or Download.</p></div>");
    } else {
        let text = String::from_utf8_lossy(&content.bytes);
        match forge_search::highlight::highlight_blob_html(&text, &path) {
            Some(html) => body.push_str(&format!("<pre class=\"hlcode\">{html}</pre>")),
            None => body.push_str(&format!("<pre>{}</pre>", esc(&text))),
        }
    }
    page(&path, &session, &body).into_response()
}

#[derive(Deserialize, Default)]
pub struct RawQuery {
    #[serde(default)]
    download: Option<String>,
}

/// GET /{org}/{repo}/raw/{rev}/{*path} — the file bytes, served as an inert
/// `text/plain` or `application/octet-stream` (never HTML) so user content
/// cannot execute in the forge origin.
pub async fn raw(
    State(state): State<AppState>,
    Path((org, repo, rev, path)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<RawQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = web_repo_read(&state, &session, &org, &repo).await {
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
    let ctype = if content.binary {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    };
    let filename: String = path
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .chars()
        .map(|c| if c == '"' || c.is_control() { '_' } else { c })
        .collect();
    let disposition = if q.download.is_some() || content.binary { "attachment" } else { "inline" };
    let mut response = Response::new(Body::from(content.bytes));
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    if let Ok(cd) = HeaderValue::from_str(&format!("{disposition}; filename=\"{filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, cd);
    }
    response
}

/// GET /{org}/{repo}/archive/{file} — `<rev>.tar.gz` source archive.
pub async fn archive(
    State(state): State<AppState>,
    Path((org, repo, file)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = web_repo_read(&state, &session, &org, &repo).await {
        return resp;
    }
    let Some(rev) = file.strip_suffix(".tar.gz") else {
        return error_page(&session, StatusCode::NOT_FOUND, "only .tar.gz archives are available");
    };
    let commit = match state.host.resolve_commit(&org, &repo, rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let safe_rev: String = rev
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let prefix = format!("{repo}-{safe_rev}");
    match state.host.archive_targz(&org, &repo, commit, &prefix) {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
            if let Ok(cd) = HeaderValue::from_str(&format!("attachment; filename=\"{prefix}.tar.gz\"")) {
                response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
            }
            response
        }
        Err(_) => error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "archive failed"),
    }
}

/// GET /{org}/{repo}/commits/{rev}
pub async fn commits(
    State(state): State<AppState>,
    Path((org, repo, rev)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match state.host.resolve_commit(&org, &repo, &rev) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "revision not found"),
    };
    let history = state.host.history(&org, &repo, commit, 100).unwrap_or_default();
    let mut body = repo_header(&org, &repo, &meta, "code", role);
    body.push_str(&format!("<div class=\"card\"><h3>Commits on {}</h3><table>", esc(&rev)));
    if history.is_empty() {
        body.push_str("<tr><td class=\"muted\">No commits.</td></tr>");
    }
    for c in history {
        body.push_str(&format!(
            "<tr><td><a href=\"/{o}/{r}/commit/{oid}\">{summary}</a><br>\
             <span class=\"muted\">{who} committed on {date}</span></td>\
             <td class=\"mono\"><a href=\"/{o}/{r}/tree/{oid}\">{short}</a></td></tr>",
            o = esc(&org),
            r = esc(&repo),
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
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match state.host.resolve_commit(&org, &repo, &oid) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "commit not found"),
    };
    let (info, diff) = match state.host.commit_detail(&org, &repo, commit) {
        Ok(v) => v,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "diff failed"),
    };
    let check_runs = state
        .store
        .latest_check_runs(meta.id, &commit.to_hex())
        .await
        .unwrap_or_default();

    let mut body = repo_header(&org, &repo, &meta, "code", role);
    body.push_str(&format!(
        "<div class=\"card\"><h2>{summary}</h2>\
         <p class=\"muted\">{who} committed on {date} \u{00b7} <span class=\"mono\">{full}</span> \u{00b7} \
         <a href=\"/{o}/{r}/tree/{full}\">Browse files</a></p>\
         <pre style=\"white-space:pre-wrap\">{message}</pre></div>",
        summary = esc(&info.summary),
        full = esc(&info.oid),
        who = esc(&info.author_name),
        date = short_date(info.time),
        message = esc(&info.message),
        o = esc(&org),
        r = esc(&repo),
    ));
    if !check_runs.is_empty() {
        body.push_str("<div class=\"card\"><h3>Checks</h3>");
        body.push_str(&check_runs_html(&org, &repo, &check_runs));
        body.push_str("</div>");
    }
    body.push_str(&render_diff_html(&diff));
    page("Commit", &session, &body).into_response()
}

/// Render a `DiffResult` as coloured unified-diff HTML.
pub fn render_diff_html(diff: &DiffResult) -> String {
    let mut out = format!(
        "<div class=\"card\"><strong>{n} file(s) changed</strong> \
         <span class=\"ok\">+{add}</span> <span class=\"bad\">-{del}</span></div>",
        n = diff.files.len(),
        add = diff.additions,
        del = diff.deletions
    );
    for file in &diff.files {
        out.push_str(&format!(
            "<div class=\"diff\"><div class=\"h\">{status} {path} \
             <span class=\"ok\">+{add}</span> <span class=\"bad\">-{del}</span></div>",
            status = esc(&file.status.to_string()),
            path = esc(&file.path),
            add = file.additions,
            del = file.deletions
        ));
        if file.binary {
            out.push_str("<pre>Binary file not shown.</pre></div>");
            continue;
        }
        out.push_str("<pre class=\"hlcode hldiff\">");
        out.push_str(&forge_search::highlight::highlight_diff_html(&file.path, &file.patch));
        out.push_str("</pre></div>");
    }
    out
}

#[derive(Deserialize)]
pub struct ForkForm {
    org: String,
    #[serde(default)]
    name: Option<String>,
}

/// POST /{org}/{repo}/fork — fork into an org where the user can create repos.
pub async fn fork(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<ForkForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(user) = session.clone() else {
        return login_redirect(&format!("/{org}/{repo}"));
    };
    let dst = match state.store.get_org(&form.org).await {
        Ok(o) => o,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "organization not found"),
    };
    match crate::authz::user_org_role(&state, &user, &dst).await {
        Some(r) if r >= Role::Admin => {}
        _ => return error_page(&session, StatusCode::FORBIDDEN, "you cannot create repositories in that organization"),
    }
    let name = form
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| repo.clone());
    match crate::api::fork_into(&state, &org, &meta, &dst, name.trim(), &user).await {
        Ok(created) => Redirect::to(&format!("/{}/{}", dst.name, created.name)).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

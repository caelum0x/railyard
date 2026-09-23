//! Release pages: list, create form + handler, and a source tarball download
//! produced on demand from the git tree via `rit`.

use axum::body::Body;
use axum::extract::{Form, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{Repository as RepoMeta, ReleaseAssetStore, ReleaseStore, Store, User};
use serde::Deserialize;

use super::{error_page, esc, page, session_user, short_date};
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

fn tabs(org: &str, repo: &str) -> String {
    format!(
        "<div class=\"row\" style=\"border-bottom:1px solid var(--line);padding-bottom:8px;margin-bottom:12px\">\
<a style=\"margin-right:14px\" href=\"/{org}/{repo}\">Code</a>\
<a style=\"margin-right:14px\" href=\"/{org}/{repo}/issues\">Issues</a>\
<a style=\"margin-right:14px\" href=\"/{org}/{repo}/pulls\">Pull requests</a>\
<a style=\"margin-right:14px;font-weight:700\" href=\"/{org}/{repo}/releases\">Releases</a></div>",
        org = esc(org),
        repo = esc(repo)
    )
}

/// GET /{org}/{repo}/releases
pub async fn list(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let releases = state.store.list_releases(meta.id).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{}/{}</h1>{}<div class=\"row\"><span class=\"spacer\"></span>\
<a class=\"btn primary\" href=\"/{org}/{repo}/releases/new\">Draft a release</a></div></div>",
        esc(&org),
        esc(&repo),
        tabs(&org, &repo)
    );
    body.push_str("<div class=\"card\">");
    if releases.is_empty() {
        body.push_str("<p class=\"muted\">No releases yet.</p>");
    }
    for r in releases {
        // Binary assets attached to this release (compiled artifacts, etc.).
        let assets = state.store.list_assets(r.id).await.unwrap_or_default();
        let mut asset_html = String::new();
        for a in &assets {
            asset_html.push_str(&format!(
                "<li><a href=\"/api/orgs/{org}/repos/{repo}/releases/{tag}/assets/{id}\">{name}</a> \
<span class=\"muted\">({size} bytes)</span></li>",
                org = esc(&org),
                repo = esc(&repo),
                tag = esc(&r.tag),
                id = a.id,
                name = esc(&a.filename),
                size = a.size_bytes,
            ));
        }
        let assets_block = if asset_html.is_empty() {
            String::new()
        } else {
            format!("<p class=\"muted\">Assets</p><ul>{asset_html}</ul>")
        };
        body.push_str(&format!(
            "<div class=\"card\"><h3>{name} <span class=\"muted\">{tag}</span></h3>\
<p class=\"muted\">{sha} · {date}</p><div>{notes}</div>\
<p><a class=\"btn\" href=\"/{org}/{repo}/releases/{tag}/archive.tar.gz\">Source (tar.gz)</a></p>\
{assets_block}</div>",
            name = esc(&r.name),
            tag = esc(&r.tag),
            sha = esc(&r.target_sha[..r.target_sha.len().min(8)]),
            date = short_date(r.created_at.unix_timestamp()),
            notes = super::markdown(r.body.as_deref().unwrap_or("")),
            org = esc(&org),
            repo = esc(&repo)
        ));
    }
    body.push_str("</div>");
    page("Releases", &session, &body).into_response()
}

/// GET /{org}/{repo}/releases/new
pub async fn new_form(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    if let Err(resp) = repo_or_404(&state, &session, &org, &repo).await {
        return resp;
    }
    let default_branch = state.host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());
    let body = format!(
        "<div class=\"card\"><h2>New release</h2>\
<form method=\"post\" action=\"/{org}/{repo}/releases/create\">\
<label>Tag</label><input name=\"tag\" placeholder=\"v1.0.0\" required>\
<label>Title</label><input name=\"name\" required>\
<label>Target (branch/tag/commit)</label><input name=\"target\" value=\"{db}\" required>\
<label>Notes</label><textarea name=\"body\" rows=\"5\"></textarea>\
<p><button class=\"btn primary\" type=\"submit\">Publish release</button></p></form></div>",
        org = esc(&org),
        repo = esc(&repo),
        db = esc(&default_branch)
    );
    page("New release", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct ReleaseForm {
    tag: String,
    name: String,
    target: String,
    #[serde(default)]
    body: String,
}

/// POST /{org}/{repo}/releases/create
pub async fn create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<ReleaseForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    // Resolve the target revision to a concrete commit sha.
    let commit = match state.host.resolve_commit(&org, &repo, &form.target) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::BAD_REQUEST, "target revision not found"),
    };
    let author = session.as_ref().map(|u| u.id);
    let notes = (!form.body.trim().is_empty()).then(|| form.body.clone());
    match state
        .store
        .create_release(meta.id, &form.tag, &form.name, notes.as_deref(), &commit.to_hex(), author)
        .await
    {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/releases")).into_response(),
        Err(_) => error_page(&session, StatusCode::BAD_REQUEST, "could not create release (tag may exist)"),
    }
}

/// GET /{org}/{repo}/releases/{tag}/archive.tar.gz
pub async fn archive(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let meta = match repo_or_404(&state, &session, &org, &repo).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let release = match state.store.get_release(meta.id, &tag).await {
        Ok(r) => r,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "release not found"),
    };
    let commit = match state.host.resolve_commit(&org, &repo, &release.target_sha) {
        Ok(c) => c,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "target commit not found"),
    };
    let prefix = format!("{repo}-{tag}");
    let bytes = match state.host.archive_targz(&org, &repo, commit, &prefix) {
        Ok(b) => b,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "archive failed"),
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    if let Ok(cd) =
        HeaderValue::from_str(&format!("attachment; filename=\"{prefix}.tar.gz\""))
    {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
    response
}

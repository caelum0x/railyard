//! Release pages: list, publish, edit, delete, binary asset upload/download/
//! delete, and an on-demand source tarball produced from the git tree.
//!
//! Reads follow repository visibility; publishing, editing, deleting, and asset
//! management require Write. Publishing goes through
//! [`crate::releases_api::publish_release`] so the `release.published` webhook
//! and notifications fire exactly as from the API.
//!
//! Asset upload streams the file as the raw request body to the JSON API
//! (`POST /api/.../releases/{tag}/assets?name=`), authenticated by the session
//! cookie — the same GitHub-style raw-body contract API clients use.

use axum::body::Body;
use axum::extract::{Form, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{NewRelease, ReleaseAssetStore, ReleaseStore, RepoRole};
use serde::Deserialize;
use uuid::Uuid;

use super::{
    app_error_page, error_page, esc, markdown, notice_html, page, repo_header, session_user,
    short_date, username_of, web_repo_read, web_repo_role,
};
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct NoticeQuery {
    #[serde(default)]
    notice: Option<String>,
}

/// Inline uploader: sends the chosen file as the raw body to the asset API and
/// reloads on success. Progressive: the form is inert without JS, and the page
/// says so.
fn upload_widget(org: &str, repo: &str, tag: &str) -> String {
    let api = format!(
        "/api/orgs/{}/repos/{}/releases/{}/assets",
        super::url_encode(org),
        super::url_encode(repo),
        super::url_encode(tag)
    );
    format!(
        "<form class=\"row asset-upload\" data-api=\"{api}\" style=\"margin-top:8px\" \
          onsubmit=\"event.preventDefault();var f=this.querySelector('input[type=file]').files[0];\
if(!f){{return;}}var s=this.querySelector('.upload-status');s.textContent='Uploading\u{2026}';\
fetch(this.dataset.api+'?name='+encodeURIComponent(f.name),{{method:'POST',credentials:'same-origin',\
headers:{{'Content-Type':f.type||'application/octet-stream'}},body:f}}).then(function(r){{\
if(r.ok){{location.reload();}}else{{r.json().then(function(j){{s.textContent=j.error||('Upload failed ('+r.status+')');}},\
function(){{s.textContent='Upload failed ('+r.status+')';}});}}}},function(){{s.textContent='Network error';}});\">\
         <input type=\"file\" required style=\"flex:1\" aria-label=\"Asset file\">\
         <button class=\"btn small\" type=\"submit\">Upload asset</button>\
         <span class=\"upload-status muted\" role=\"status\"></span>\
         <noscript><span class=\"muted\">Asset upload needs JavaScript, or use the API.</span></noscript></form>",
        api = esc(&api),
    )
}

/// GET /{org}/{repo}/releases
pub async fn list(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, role) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let can_write = role.is_some_and(|r| r.can_write());
    let releases = state.store.list_releases(meta.id).await.unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let mut body = notice_html(q.notice.as_deref());
    body.push_str(&repo_header(&org, &repo, &meta, "releases", role));
    if can_write {
        body.push_str(&format!(
            "<div class=\"row\" style=\"margin-bottom:12px\"><span class=\"spacer\"></span>\
             <a class=\"btn primary\" href=\"/{o}/{r}/releases/new\">Draft a new release</a></div>"
        ));
    }
    if releases.is_empty() {
        body.push_str(
            "<div class=\"card\"><div class=\"empty-state\"><h3>No releases yet</h3>\
             <p>Releases package a tagged snapshot of the code with notes and downloadable assets.</p></div></div>",
        );
        return page("Releases", &session, &body).into_response();
    }
    for rel in releases {
        let t = esc(&rel.tag);
        let tag_path = super::url_encode(&rel.tag);
        let assets = state.store.list_assets(rel.id).await.unwrap_or_default();
        let mut asset_rows = String::new();
        for a in &assets {
            let delete = if can_write {
                format!(
                    "<form method=\"post\" action=\"/{o}/{r}/releases/{tag_path}/assets/{id}/delete\" class=\"inline-form\">\
                     <button class=\"btn small danger\" type=\"submit\">Delete</button></form>",
                    id = a.id
                )
            } else {
                String::new()
            };
            asset_rows.push_str(&format!(
                "<tr><td><a href=\"/{o}/{r}/releases/{tag_path}/assets/{id}\" hx-boost=\"false\">{name}</a></td>\
                 <td class=\"muted\">{size}</td><td>{delete}</td></tr>",
                id = a.id,
                name = esc(&a.filename),
                size = human_size(a.size_bytes),
            ));
        }
        let manage = if can_write {
            format!(
                "<a class=\"btn small\" href=\"/{o}/{r}/releases/{tag_path}/edit\">Edit</a>\
                 <form method=\"post\" action=\"/{o}/{r}/releases/{tag_path}/delete\" class=\"inline-form\" \
                 onsubmit=\"return confirm('Delete this release and its assets?')\">\
                 <button class=\"btn small danger\" type=\"submit\">Delete</button></form>"
            )
        } else {
            String::new()
        };
        body.push_str(&format!(
            "<div class=\"card\"><div class=\"row\"><h2>{name}</h2><span class=\"pill\">{t}</span>\
             <span class=\"spacer\"></span>{manage}</div>\
             <p class=\"muted\">{who} released this on {date} \u{00b7} \
             <a href=\"/{o}/{r}/commit/{sha}\" class=\"mono\">{short}</a></p>\
             <div class=\"readme\">{notes}</div>\
             <h3>Assets</h3><table>\
             <tr><td><a href=\"/{o}/{r}/releases/{tag_path}/archive.tar.gz\" hx-boost=\"false\">Source code (tar.gz)</a></td><td></td><td></td></tr>\
             {asset_rows}</table>{upload}</div>",
            name = esc(&rel.name),
            who = esc(&username_of(&state, rel.author_id).await),
            date = short_date(rel.created_at.unix_timestamp()),
            sha = esc(&rel.target_sha),
            short = esc(&rel.target_sha[..rel.target_sha.len().min(8)]),
            notes = markdown(rel.body.as_deref().unwrap_or("")),
            upload = if can_write { upload_widget(&org, &repo, &rel.tag) } else { String::new() },
        ));
    }
    page("Releases", &session, &body).into_response()
}

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// GET /{org}/{repo}/releases/new (Write)
pub async fn new_form(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases/new");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let default_branch = state.host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());
    let mut body = repo_header(&org, &repo, &meta, "releases", Some(RepoRole::Write));
    body.push_str(&format!(
        "<div class=\"card\"><h2>New release</h2>\
         <form method=\"post\" action=\"/{o}/{r}/releases/create\">\
         <div class=\"grid2\"><div><label for=\"rl-tag\">Tag</label>\
         <input id=\"rl-tag\" name=\"tag\" placeholder=\"v1.0.0\" required maxlength=\"128\"></div>\
         <div><label for=\"rl-target\">Target (branch, tag, or commit)</label>\
         <input id=\"rl-target\" name=\"target\" value=\"{db}\" required></div></div>\
         <label for=\"rl-name\">Title</label>\
         <input id=\"rl-name\" name=\"name\" required maxlength=\"200\" placeholder=\"Release name\">\
         <label for=\"rl-body\">Release notes</label>\
         <textarea id=\"rl-body\" name=\"body\" rows=\"8\" placeholder=\"What changed in this release. Markdown supported.\"></textarea>\
         <p class=\"hint\">Attach binaries from the releases page after publishing.</p>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Publish release</button></p>\
         </form></div>",
        o = esc(&org),
        r = esc(&repo),
        db = esc(&default_branch)
    ));
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

/// POST /{org}/{repo}/releases/create (Write)
pub async fn create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<ReleaseForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases/new");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = NewRelease {
        tag: form.tag,
        name: form.name,
        body: Some(form.body),
        target: form.target,
    };
    match crate::releases_api::publish_release(&state, &org, &repo, &meta, &user, &input).await {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/releases?notice=created")).into_response(),
        Err(e) if e.status() == StatusCode::CONFLICT => {
            error_page(&session, StatusCode::CONFLICT, "a release with that tag already exists")
        }
        Err(e) => app_error_page(&session, e),
    }
}

/// GET /{org}/{repo}/releases/{tag}/edit (Write)
pub async fn edit_form(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases");
    let (meta, _) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let release = match state.store.get_release(meta.id, &tag).await {
        Ok(r) => r,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "release not found"),
    };
    let mut body = repo_header(&org, &repo, &meta, "releases", Some(RepoRole::Write));
    body.push_str(&format!(
        "<div class=\"card\"><h2>Edit release {t}</h2>\
         <form method=\"post\" action=\"/{o}/{r}/releases/{tp}/edit\">\
         <label for=\"re-name\">Title</label>\
         <input id=\"re-name\" name=\"name\" value=\"{name}\" required maxlength=\"200\">\
         <label for=\"re-body\">Release notes</label>\
         <textarea id=\"re-body\" name=\"body\" rows=\"10\">{notes}</textarea>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Save release</button> \
         <a class=\"btn\" href=\"/{o}/{r}/releases\">Cancel</a></p></form></div>",
        t = esc(&release.tag),
        tp = super::url_encode(&release.tag),
        o = esc(&org),
        r = esc(&repo),
        name = esc(&release.name),
        notes = esc(release.body.as_deref().unwrap_or("")),
    ));
    page("Edit release", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct EditForm {
    name: String,
    #[serde(default)]
    body: String,
}

/// POST /{org}/{repo}/releases/{tag}/edit (Write)
pub async fn edit_submit(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
    Form(form): Form<EditForm>,
) -> Response {
    use rithub_core::DirectoryStore;
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if form.name.trim().is_empty() {
        return error_page(&session, StatusCode::BAD_REQUEST, "release title is required");
    }
    let notes = Some(form.body.trim()).filter(|b| !b.is_empty());
    match state.store.update_release(meta.id, &tag, form.name.trim(), notes).await {
        Ok(_) => {
            crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "release.edited", Some(&tag), None)
                .await;
            Redirect::to(&format!("{next}?notice=saved")).into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

/// POST /{org}/{repo}/releases/{tag}/delete (Write)
pub async fn delete(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match crate::releases_api::remove_release(&state, &meta, &user, &tag).await {
        Ok(()) => Redirect::to(&format!("{next}?notice=deleted")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// GET /{org}/{repo}/releases/{tag}/assets/{id} — download (read access).
pub async fn asset_download(
    State(state): State<AppState>,
    Path((org, repo, tag, id)): Path<(String, String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let asset = match state.store.get_release(meta.id, &tag).await {
        Ok(rel) => state.store.get_asset(rel.id, id).await,
        Err(e) => Err(e),
    };
    let Ok(asset) = asset else {
        return error_page(&session, StatusCode::NOT_FOUND, "asset not found");
    };
    let bytes = match tokio::fs::read(&asset.storage_path).await {
        Ok(b) => b,
        Err(_) => return error_page(&session, StatusCode::NOT_FOUND, "asset file is missing"),
    };
    let safe_name: String = asset
        .filename
        .chars()
        .map(|c| if c == '"' || c.is_control() { '_' } else { c })
        .collect();
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    if let Ok(cd) = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\"")) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
    response
}

/// POST /{org}/{repo}/releases/{tag}/assets/{id}/delete (Write)
pub async fn asset_delete(
    State(state): State<AppState>,
    Path((org, repo, tag, id)): Path<(String, String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let next = format!("/{org}/{repo}/releases");
    let (meta, user) = match web_repo_role(&state, &session, &org, &repo, RepoRole::Write, &next).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let result = async {
        let rel = state.store.get_release(meta.id, &tag).await?;
        state.store.delete_asset(rel.id, id).await
    }
    .await;
    match result {
        Ok(asset) => {
            let _ = tokio::fs::remove_file(&asset.storage_path).await;
            crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "asset.deleted", Some(&asset.filename), None)
                .await;
            Redirect::to(&format!("{next}?notice=deleted")).into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

/// GET /{org}/{repo}/releases/{tag}/archive.tar.gz (read access)
pub async fn archive(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match web_repo_read(&state, &session, &org, &repo).await {
        Ok(v) => v,
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
    let safe_tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let prefix = format!("{repo}-{safe_tag}");
    let bytes = match state.host.archive_targz(&org, &repo, commit, &prefix) {
        Ok(b) => b,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "archive failed"),
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    if let Ok(cd) = HeaderValue::from_str(&format!("attachment; filename=\"{prefix}.tar.gz\"")) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
    response
}

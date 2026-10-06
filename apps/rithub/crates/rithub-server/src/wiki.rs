//! Repository wikis: markdown pages in a git repository of their own
//! (`{org}/{repo}.wiki.git`, clonable and pushable like any repo), with page
//! history and the parent repository's permissions (read to view, write to
//! edit).
//!
//! | Method + path (under `/api/orgs/{org}/repos/{repo}/wiki`) | Purpose |
//! |---|---|
//! | `GET /` | list pages |
//! | `GET /{page}[?rev=<sha>]` | a page: markdown, rendered HTML, revision |
//! | `PUT /{page}` `{content, message?, revision?}` | create or edit (revision = optimistic lock) |
//! | `DELETE /{page}` | delete |
//! | `GET /{page}/history` | commits that changed the page |

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use rithub_core::RepoRole;
use rithub_git::wiki::FileEdit;
use rithub_git::SeedFile;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::authz::{repo_for_read, repo_for_role};
use crate::error::AppError;
use crate::state::AppState;

/// The wiki branch.
pub const BRANCH: &str = "main";
/// The largest page accepted.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;

pub fn wiki_repo(repo: &str) -> String {
    format!("{repo}.wiki")
}

/// A page name: letters, digits, space, `-` and `_` (1-100); stored as `Name-With-Dashes.md`.
pub fn page_file(name: &str) -> Result<String, AppError> {
    let ok = !name.trim().is_empty() && name.len() <= 100 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_');
    if !ok {
        return Err(AppError::bad_request("page names may use letters, digits, spaces, - and _ (1-100 characters)"));
    }
    Ok(format!("{}.md", name.trim().replace(' ', "-")))
}

fn title_of(file: &str) -> String {
    file.trim_end_matches(".md").replace('-', " ")
}

fn head(state: &AppState, org: &str, repo: &str) -> Option<rithub_git::ObjectId> {
    let wiki = wiki_repo(repo);
    if !state.host.exists(org, &wiki).ok()? {
        return None;
    }
    state.host.resolve_branch(org, &wiki, BRANCH).ok()
}

/// `GET .../wiki`
pub async fn list_pages(State(state): State<AppState>, Path((org, repo)): Path<(String, String)>, headers: HeaderMap) -> Result<Json<Value>, AppError> {
    repo_for_read(&state, &headers, &org, &repo).await?;
    let Some(tip) = head(&state, &org, &repo) else {
        return Ok(Json(json!({ "pages": [], "revision": null })));
    };
    let items = state.host.list_tree(&org, &wiki_repo(&repo), tip, "")?;
    let pages: Vec<Value> = items
        .iter()
        .filter(|i| !i.is_dir && i.name.ends_with(".md"))
        .map(|i| json!({ "name": i.name.trim_end_matches(".md"), "title": title_of(&i.name) }))
        .collect();
    Ok(Json(json!({ "pages": pages, "revision": tip.to_hex() })))
}

#[derive(Debug, Deserialize)]
pub struct RevQuery {
    pub rev: Option<String>,
}

/// `GET .../wiki/{page}`
pub async fn get_page(State(state): State<AppState>, Path((org, repo, page)): Path<(String, String, String)>, Query(q): Query<RevQuery>, headers: HeaderMap) -> Result<Json<Value>, AppError> {
    repo_for_read(&state, &headers, &org, &repo).await?;
    let file = page_file(&page)?;
    let wiki = wiki_repo(&repo);
    let tip = head(&state, &org, &repo).ok_or_else(|| AppError::not_found("this repository has no wiki yet"))?;
    let at = match q.rev.as_deref() {
        Some(rev) => state.host.resolve_commit(&org, &wiki, rev)?,
        None => tip,
    };
    let blob = state.host.read_blob_at(&org, &wiki, at, &file).map_err(|_| AppError::not_found(format!("page not found: {page}")))?;
    let content = String::from_utf8_lossy(&blob.bytes).into_owned();
    Ok(Json(json!({
        "name": file.trim_end_matches(".md"),
        "title": title_of(&file),
        "content": content,
        "html": crate::web::markdown(&content),
        "revision": at.to_hex(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct PageInput {
    pub content: String,
    #[serde(default)]
    pub message: Option<String>,
    /// The wiki revision the edit was based on; a stale one is refused.
    #[serde(default)]
    pub revision: Option<String>,
}

/// `PUT .../wiki/{page}`
pub async fn put_page(State(state): State<AppState>, Path((org, repo, page)): Path<(String, String, String)>, headers: HeaderMap, Json(input): Json<PageInput>) -> Result<Json<Value>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let file = page_file(&page)?;
    if input.content.len() > MAX_PAGE_BYTES {
        return Err(AppError::bad_request("the page is too large"));
    }
    let wiki = wiki_repo(&repo);
    if !state.host.exists(&org, &wiki)? {
        state.host.init_bare(&org, &wiki, BRANCH)?;
    }
    let message = input.message.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| format!("Update {}", title_of(&file)));
    let upserts = [SeedFile { name: file.clone(), content: input.content.into_bytes(), executable: false }];
    let revision = state
        .host
        .commit_files(&org, &wiki, BRANCH, FileEdit { upserts: &upserts, deletes: &[] }, input.revision.as_deref(), &message, &user.username, &user.email)
        .map_err(|e| match e {
            rithub_git::GitHostError::Invalid(m) if m.contains("changed since") => AppError::conflict(m),
            other => AppError::from(other),
        })?;
    crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "wiki.page_saved", Some(&page), None).await;
    Ok(Json(json!({ "name": file.trim_end_matches(".md"), "revision": revision })))
}

/// `DELETE .../wiki/{page}`
pub async fn delete_page(State(state): State<AppState>, Path((org, repo, page)): Path<(String, String, String)>, headers: HeaderMap) -> Result<Json<Value>, AppError> {
    let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
    let file = page_file(&page)?;
    head(&state, &org, &repo).ok_or_else(|| AppError::not_found("this repository has no wiki yet"))?;
    let revision = state.host.commit_files(
        &org,
        &wiki_repo(&repo),
        BRANCH,
        FileEdit { upserts: &[], deletes: &[file.clone()] },
        None,
        &format!("Delete {}", title_of(&file)),
        &user.username,
        &user.email,
    )?;
    crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "wiki.page_deleted", Some(&page), None).await;
    Ok(Json(json!({ "deleted": file.trim_end_matches(".md"), "revision": revision })))
}

/// `GET .../wiki/{page}/history`
pub async fn page_history(State(state): State<AppState>, Path((org, repo, page)): Path<(String, String, String)>, headers: HeaderMap) -> Result<Json<Value>, AppError> {
    repo_for_read(&state, &headers, &org, &repo).await?;
    let file = page_file(&page)?;
    let tip = head(&state, &org, &repo).ok_or_else(|| AppError::not_found("this repository has no wiki yet"))?;
    let commits = state.host.file_history(&org, &wiki_repo(&repo), tip, &file, 100)?;
    let out: Vec<Value> = commits
        .iter()
        .map(|c| json!({ "revision": c.oid, "summary": c.summary, "author": c.author_name, "time": c.time }))
        .collect();
    Ok(Json(json!({ "page": file.trim_end_matches(".md"), "history": out })))
}

#[cfg(test)]
mod tests {
    use super::page_file;

    #[test]
    fn page_names_map_to_flat_markdown_files() {
        assert_eq!(page_file("Getting Started").unwrap(), "Getting-Started.md");
        assert!(page_file("../etc/passwd").is_err());
        assert!(page_file(".hidden").is_err());
        assert!(page_file("  ").is_err());
    }
}

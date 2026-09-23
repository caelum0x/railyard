//! Repository settings (Admin only): general (description, default branch,
//! visibility, rename), danger zone (delete), access (collaborators + team
//! grants overview), branch protection, webhooks (create / edit / toggle /
//! delete + recent deliveries), and deploy keys.
//!
//! Every form posts to a handler that reuses the API's validated service
//! functions ([`crate::api`], [`crate::collab_api`], [`crate::governance_api`]),
//! so the SSRF guard, plan quota, audit trail, and billing metering apply
//! identically from the browser.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{
    DeployKeyStore, DirectoryStore, NewWebhook, RepoRole, Repository, ReviewStore, UpdateRepository,
    Visibility, WebhookPatch, WebhookStore,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{
    app_error_page, error_page, esc, notice_html, page, repo_header, session_user, short_date,
    web_repo_role,
};
use crate::state::AppState;

/// Events a webhook may subscribe to.
const HOOK_EVENTS: &[&str] = &[
    "push", "issue.opened", "issue.closed", "issue.reopened", "issue.commented", "issue.updated",
    "pr.opened", "pr.closed", "pr.reopened", "pr.commented", "pr.reviewed", "pr.merged",
    "release.published", "check_run",
];

#[derive(Deserialize, Default)]
pub struct NoticeQuery {
    #[serde(default)]
    notice: Option<String>,
}

fn settings_nav(org: &str, repo: &str, active: &str) -> String {
    let (o, r) = (esc(org), esc(repo));
    let item = |key: &str, path: &str, label: &str| {
        let cls = if key == active { " class=\"active\"" } else { "" };
        format!("<a href=\"/{o}/{r}/settings{path}\"{cls}>{label}</a>")
    };
    format!(
        "<nav class=\"subnav\">{}{}{}{}{}<a href=\"/{o}/{r}/audit\">Audit log</a></nav>",
        item("general", "", "General"),
        item("access", "/access", "Collaborators"),
        item("branches", "/branches", "Branches"),
        item("hooks", "/hooks", "Webhooks"),
        item("keys", "/keys", "Deploy keys"),
    )
}

async fn admin_repo(
    state: &AppState,
    session: &Option<rithub_core::User>,
    org: &str,
    repo: &str,
    section: &str,
) -> Result<(Repository, rithub_core::User), Response> {
    let next = format!("/{org}/{repo}/settings{section}");
    web_repo_role(state, session, org, repo, RepoRole::Admin, &next).await
}

fn shell(org: &str, repo: &str, meta: &Repository, active: &str, notice: Option<&str>, inner: &str) -> String {
    format!(
        "{notice}{header}<div class=\"card\">{nav}{inner}</div>",
        notice = notice_html(notice),
        header = repo_header(org, repo, meta, "settings", Some(RepoRole::Admin)),
        nav = settings_nav(org, repo, active),
    )
}

// --- general + danger zone ----------------------------------------------------

/// GET /{org}/{repo}/settings
pub async fn general_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match admin_repo(&state, &session, &org, &repo, "").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let branches = state.host.branches(&org, &repo).unwrap_or_default();
    let current_default = state.host.default_branch(&org, &repo).unwrap_or_else(|_| meta.default_branch.clone());
    let branch_opts: String = if branches.is_empty() {
        format!("<option value=\"{b}\" selected>{b}</option>", b = esc(&current_default))
    } else {
        branches
            .iter()
            .map(|b| {
                let sel = if *b == current_default { " selected" } else { "" };
                format!("<option value=\"{v}\"{sel}>{v}</option>", v = esc(b))
            })
            .collect()
    };
    let private = meta.visibility == Visibility::Private;
    let (o, r) = (esc(&org), esc(&repo));
    let inner = format!(
        "<h3>General</h3>\
         <form method=\"post\" action=\"/{o}/{r}/settings/general\">\
         <label for=\"s-name\">Repository name</label>\
         <input id=\"s-name\" name=\"name\" value=\"{r}\" required maxlength=\"100\">\
         <p class=\"hint\">Renaming changes the clone URL; update your git remotes afterwards.</p>\
         <label for=\"s-desc\">Description</label>\
         <input id=\"s-desc\" name=\"description\" value=\"{desc}\" maxlength=\"350\">\
         <label for=\"s-branch\">Default branch</label>\
         <select id=\"s-branch\" name=\"default_branch\">{branch_opts}</select>\
         <label>Visibility</label>\
         <label class=\"check\"><input type=\"radio\" name=\"visibility\" value=\"public\"{pub_c}> Public</label>\
         <label class=\"check\"><input type=\"radio\" name=\"visibility\" value=\"private\"{priv_c}> Private</label>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Save changes</button></p></form>\
         <hr><h3 class=\"bad\">Danger zone</h3>\
         <form method=\"post\" action=\"/{o}/{r}/settings/delete\" class=\"card danger-zone\">\
         <p>Deleting <strong>{o}/{r}</strong> permanently removes its git data, issues, pull requests, \
         releases, and settings. This cannot be undone.</p>\
         <label for=\"s-confirm\">Type <code>{o}/{r}</code> to confirm</label>\
         <input id=\"s-confirm\" name=\"confirm\" required autocomplete=\"off\">\
         <p style=\"margin-top:10px\"><button class=\"btn danger\" type=\"submit\">Delete this repository</button></p></form>",
        desc = esc(meta.description.as_deref().unwrap_or("")),
        pub_c = if private { "" } else { " checked" },
        priv_c = if private { " checked" } else { "" },
    );
    let body = shell(&org, &repo, &meta, "general", q.notice.as_deref(), &inner);
    page("Settings", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct GeneralForm {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    visibility: String,
}

/// POST /{org}/{repo}/settings/general
pub async fn general_submit(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<GeneralForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let current_default = state.host.default_branch(&org, &repo).unwrap_or_else(|_| meta.default_branch.clone());
    let new_name = form.name.trim().to_string();
    let input = UpdateRepository {
        name: (new_name != meta.name).then_some(new_name),
        description: Some(form.description.trim().to_string()),
        default_branch: Some(form.default_branch.trim().to_string())
            .filter(|b| !b.is_empty() && *b != current_default),
        visibility: Some(if form.visibility == "private" { Visibility::Private } else { Visibility::Public }),
    };
    match crate::api::update_repo_settings(&state, &org, &meta, &user, &input).await {
        Ok(updated) => Redirect::to(&format!("/{org}/{}/settings?notice=saved", updated.name)).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct DeleteForm {
    confirm: String,
}

/// POST /{org}/{repo}/settings/delete
pub async fn delete_submit(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if form.confirm.trim() != format!("{org}/{repo}") {
        return error_page(&session, StatusCode::BAD_REQUEST, "confirmation text did not match; repository not deleted");
    }
    match crate::api::delete_repo_fully(&state, &org, &meta, &user).await {
        Ok(()) => Redirect::to(&format!("/{org}?notice=deleted")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

// --- access ------------------------------------------------------------------

/// GET /{org}/{repo}/settings/access
pub async fn access_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match admin_repo(&state, &session, &org, &repo, "/access").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let collaborators = state.store.list_collaborator_users(meta.id).await.unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let mut rows = String::new();
    for (user, role) in &collaborators {
        rows.push_str(&format!(
            "<tr><td><a href=\"/{u}\">{u}</a></td><td><span class=\"pill\">{role}</span></td>\
             <td><form method=\"post\" action=\"/{o}/{r}/settings/access/{u}/remove\" class=\"inline-form\">\
             <button class=\"btn small danger\" type=\"submit\">Remove</button></form></td></tr>",
            u = esc(&user.username),
            role = role.as_str(),
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td class=\"muted\">No direct collaborators.</td></tr>".into();
    }
    let inner = format!(
        "<h3>Collaborators</h3>\
         <p class=\"muted\">Organization owners and admins have admin access; members have read access. \
         Collaborators and <a href=\"/{o}\">team grants</a> add to that.</p>\
         <table>{rows}</table>\
         <form method=\"post\" action=\"/{o}/{r}/settings/access\" class=\"row\" style=\"margin-top:12px\">\
         <input name=\"username\" placeholder=\"username\" required style=\"flex:2;min-width:140px\" aria-label=\"Username\">\
         <select name=\"role\" style=\"flex:1;min-width:100px\" aria-label=\"Role\">\
         <option value=\"read\">read</option><option value=\"write\" selected>write</option><option value=\"admin\">admin</option></select>\
         <button class=\"btn\" type=\"submit\">Add collaborator</button></form>"
    );
    let body = shell(&org, &repo, &meta, "access", q.notice.as_deref(), &inner);
    page("Collaborators", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct CollaboratorForm {
    username: String,
    role: String,
}

/// POST /{org}/{repo}/settings/access
pub async fn collaborator_add(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<CollaboratorForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/access").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let role = match crate::governance_api::parse_repo_role(&form.role) {
        Ok(r) => r,
        Err(e) => return app_error_page(&session, e),
    };
    match crate::governance_api::grant_collaborator(&state, &meta, &user, &form.username, role).await {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/settings/access?notice=added")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/settings/access/{user}/remove
pub async fn collaborator_remove(
    State(state): State<AppState>,
    Path((org, repo, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/access").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::governance_api::revoke_collaborator(&state, &meta, &user, &username).await {
        Ok(()) => Redirect::to(&format!("/{org}/{repo}/settings/access?notice=removed")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

// --- branch protection ----------------------------------------------------------

/// GET /{org}/{repo}/settings/branches
pub async fn branches_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match admin_repo(&state, &session, &org, &repo, "/branches").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let branches = match crate::branches::list_branches(&state, &org, &meta).await {
        Ok(b) => b,
        Err(e) => return app_error_page(&session, e),
    };
    let (o, r) = (esc(&org), esc(&repo));
    let mut rows = String::new();
    for info in &branches {
        let b = &info.name;
        let p = state.store.get_branch_protection(meta.id, b).await.ok().flatten();
        let summary = match &p {
            Some(p) => format!(
                "{} approval(s){}{}",
                p.required_approvals,
                if p.block_direct_push { " \u{00b7} direct pushes blocked" } else { "" },
                if p.required_checks.is_empty() {
                    String::new()
                } else {
                    format!(" \u{00b7} checks: {}", esc(&p.required_checks.join(", ")))
                }
            ),
            None => "<span class=\"muted\">Not protected</span>".to_string(),
        };
        let (approvals, block, checks) = p
            .as_ref()
            .map(|p| (p.required_approvals, p.block_direct_push, p.required_checks.join(", ")))
            .unwrap_or((1, true, String::new()));
        let tip = info
            .sha
            .as_deref()
            .map(|sha| format!(" <span class=\"muted mono\">{}</span>", esc(&sha[..sha.len().min(10)])))
            .unwrap_or_default();
        let badge = if info.default {
            " <span class=\"label-chip\" style=\"background:#eef;color:#223\">default</span>"
        } else {
            ""
        };
        // Secondary actions: remove the rule (protected) / delete the branch
        // (never the default branch; protected branches must be unprotected first).
        let unprotect = if p.is_some() {
            format!(
                "<form method=\"post\" action=\"/{o}/{r}/settings/branches/unprotect\" style=\"display:inline\">\
                 <input type=\"hidden\" name=\"branch\" value=\"{bn}\">\
                 <button class=\"btn small\" type=\"submit\">Remove protection</button></form>",
                bn = esc(b)
            )
        } else {
            String::new()
        };
        let delete = if !info.default && p.is_none() {
            format!(
                "<form method=\"post\" action=\"/{o}/{r}/settings/branches/delete\" style=\"display:inline\" \
                 onsubmit=\"return confirm('Delete branch {bn}? This cannot be undone.')\">\
                 <input type=\"hidden\" name=\"branch\" value=\"{bn}\">\
                 <button class=\"btn small danger\" type=\"submit\">Delete branch</button></form>",
                bn = esc(b)
            )
        } else {
            String::new()
        };
        rows.push_str(&format!(
            "<div class=\"comment\"><div class=\"comment-h row\" style=\"justify-content:space-between\">\
             <span><strong class=\"mono\">{bn}</strong>{badge}{tip} \u{2014} {summary}</span>\
             <span class=\"row\">{unprotect}{delete}</span></div>\
             <div class=\"comment-b\"><form method=\"post\" action=\"/{o}/{r}/settings/branches\" class=\"row\">\
             <input type=\"hidden\" name=\"branch\" value=\"{bn}\">\
             <label style=\"margin:0\">Required approvals <input name=\"required_approvals\" type=\"number\" min=\"0\" max=\"20\" value=\"{approvals}\" style=\"width:70px\"></label>\
             <label class=\"check\" style=\"margin:0\"><input type=\"checkbox\" name=\"block_direct_push\" value=\"1\"{bc}> Block direct pushes</label>\
             <input name=\"required_checks\" value=\"{checks}\" placeholder=\"required checks, comma-separated\" style=\"flex:1;min-width:180px\" aria-label=\"Required checks\">\
             <button class=\"btn small\" type=\"submit\">Save rule</button></form></div></div>",
            bn = esc(b),
            bc = if block { " checked" } else { "" },
            checks = esc(&checks),
        ));
    }
    if rows.is_empty() {
        rows = "<p class=\"muted\">Push a branch to configure protection rules.</p>".into();
    }
    let inner = format!(
        "<h3>Branches</h3>\
         <p class=\"muted\">Protected branches can require approving reviews and passing status checks \
         before merge, and can block direct pushes (admins may still push).</p>{rows}\
         <p class=\"hint\">The default branch and protected branches cannot be deleted. Branches used by an \
         open pull request are kept until it is merged or closed.</p>"
    );
    let body = shell(&org, &repo, &meta, "branches", q.notice.as_deref(), &inner);
    page("Branches", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct ProtectForm {
    branch: String,
    #[serde(default)]
    required_approvals: i32,
    #[serde(default)]
    block_direct_push: Option<String>,
    #[serde(default)]
    required_checks: String,
}

/// POST /{org}/{repo}/settings/branches
pub async fn branch_protect(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<ProtectForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/branches").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if state.host.resolve_branch(&org, &repo, &form.branch).is_err() {
        return error_page(&session, StatusCode::BAD_REQUEST, "branch not found");
    }
    let input = crate::collab_api::ProtectionInput {
        required_approvals: form.required_approvals,
        block_direct_push: form.block_direct_push.is_some(),
        required_checks: form.required_checks.split(',').map(|s| s.trim().to_string()).collect(),
    };
    match crate::collab_api::apply_protection(&state, &meta, &user, &form.branch, &input).await {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/settings/branches?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct BranchForm {
    branch: String,
}

/// POST /{org}/{repo}/settings/branches/unprotect
pub async fn branch_unprotect(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<BranchForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/branches").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::branches::remove_protection(&state, &meta, &user, &form.branch).await {
        Ok(()) => Redirect::to(&format!("/{org}/{repo}/settings/branches?notice=unprotected"))
            .into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/settings/branches/delete
pub async fn branch_delete(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<BranchForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/branches").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::branches::delete_branch(&state, &org, &meta, &user, &form.branch).await {
        Ok(()) => Redirect::to(&format!("/{org}/{repo}/settings/branches?notice=branch-deleted"))
            .into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

// --- webhooks -------------------------------------------------------------------

fn event_checkboxes(selected: &[String]) -> String {
    HOOK_EVENTS
        .iter()
        .map(|ev| {
            let checked = if selected.iter().any(|s| s == ev) { " checked" } else { "" };
            format!("<label class=\"check\" style=\"display:inline-flex;margin-right:12px\"><input type=\"checkbox\" name=\"event\" value=\"{ev}\"{checked}> <code>{ev}</code></label>")
        })
        .collect()
}

/// GET /{org}/{repo}/settings/hooks
pub async fn hooks_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match admin_repo(&state, &session, &org, &repo, "/hooks").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let hooks = state.store.list_webhooks(meta.id).await.unwrap_or_default();
    let (o, r) = (esc(&org), esc(&repo));
    let mut list = String::new();
    for hook in &hooks {
        let deliveries = state.store.list_webhook_deliveries(hook.id, 5).await.unwrap_or_default();
        let recent: String = if deliveries.is_empty() {
            "<p class=\"muted\">No deliveries yet.</p>".into()
        } else {
            let mut t = String::from("<table>");
            for d in &deliveries {
                t.push_str(&format!(
                    "<tr><td><code>{ev}</code></td><td class=\"{cls}\">{res}</td><td class=\"muted\">{when}</td><td class=\"muted\">{err}</td></tr>",
                    ev = esc(&d.event),
                    cls = if d.success { "ok" } else { "bad" },
                    res = d.status_code.map(|c| c.to_string()).unwrap_or_else(|| if d.success { "ok".into() } else { "failed".into() }),
                    when = short_date(d.created_at.unix_timestamp()),
                    err = esc(d.error.as_deref().unwrap_or("")),
                ));
            }
            t.push_str("</table>");
            t
        };
        list.push_str(&format!(
            "<div class=\"comment\"><div class=\"comment-h row\"><strong class=\"mono\">{url}</strong>\
             <span class=\"pill {state_cls}\">{state}</span><span class=\"spacer\"></span>\
             <form method=\"post\" action=\"/{o}/{r}/settings/hooks/{id}/delete\" class=\"inline-form\">\
             <button class=\"btn small danger\" type=\"submit\">Delete</button></form></div>\
             <div class=\"comment-b\"><form method=\"post\" action=\"/{o}/{r}/settings/hooks/{id}\">\
             <label>Payload URL</label><input name=\"url\" value=\"{url}\" required>\
             <label>New secret <span class=\"muted\">(leave blank to keep the current one)</span></label>\
             <input name=\"secret\" type=\"password\" autocomplete=\"new-password\" minlength=\"8\">\
             <label>Events</label><div>{events}</div>\
             <label class=\"check\"><input type=\"checkbox\" name=\"active\" value=\"1\"{active}> Active</label>\
             <p><button class=\"btn small\" type=\"submit\">Update webhook</button></p></form>\
             <h4>Recent deliveries</h4>{recent}</div></div>",
            url = esc(&hook.url),
            id = hook.id,
            state = if hook.active { "active" } else { "disabled" },
            state_cls = if hook.active { "open" } else { "closed" },
            events = event_checkboxes(&hook.events),
            active = if hook.active { " checked" } else { "" },
        ));
    }
    if list.is_empty() {
        list = "<p class=\"muted\">No webhooks. Webhooks POST a signed JSON payload (header \
                <code>X-Rithub-Signature-256</code>, HMAC-SHA256 of the body) when events happen.</p>"
            .into();
    }
    let inner = format!(
        "<h3>Webhooks</h3>{list}<h3>Add webhook</h3>\
         <form method=\"post\" action=\"/{o}/{r}/settings/hooks\">\
         <label for=\"h-url\">Payload URL</label><input id=\"h-url\" name=\"url\" type=\"url\" required placeholder=\"https://example.com/rithub-hook\">\
         <label for=\"h-secret\">Secret</label><input id=\"h-secret\" name=\"secret\" type=\"password\" required minlength=\"8\" autocomplete=\"new-password\">\
         <p class=\"hint\">Used to sign each delivery. Receivers verify <code>X-Rithub-Signature-256</code>.</p>\
         <label>Events</label><div>{events}</div>\
         <p style=\"margin-top:10px\"><button class=\"btn primary\" type=\"submit\">Add webhook</button></p></form>",
        events = event_checkboxes(&["push".to_string(), "pr.opened".to_string(), "pr.merged".to_string()]),
    );
    let body = shell(&org, &repo, &meta, "hooks", q.notice.as_deref(), &inner);
    page("Webhooks", &session, &body).into_response()
}

fn events_from(pairs: &[(String, String)]) -> Vec<String> {
    pairs
        .iter()
        .filter(|(k, v)| k == "event" && HOOK_EVENTS.contains(&v.as_str()))
        .map(|(_, v)| v.clone())
        .collect()
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// POST /{org}/{repo}/settings/hooks — repeated `event` fields, decoded manually.
pub async fn hook_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/hooks").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let pairs = super::collab::parse_form_pairs(&body);
    let events = events_from(&pairs);
    if events.is_empty() {
        return error_page(&session, StatusCode::BAD_REQUEST, "select at least one event");
    }
    let input = NewWebhook {
        url: field(&pairs, "url").unwrap_or_default().trim().to_string(),
        secret: field(&pairs, "secret").unwrap_or_default().to_string(),
        events,
    };
    match crate::collab_api::register_hook(&state, &meta, &user, &input).await {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/settings/hooks?notice=created")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/settings/hooks/{id} — update URL/secret/events/active.
pub async fn hook_update(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/hooks").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let pairs = super::collab::parse_form_pairs(&body);
    let events = events_from(&pairs);
    if events.is_empty() {
        return error_page(&session, StatusCode::BAD_REQUEST, "select at least one event");
    }
    let patch = WebhookPatch {
        url: field(&pairs, "url").map(|u| u.trim().to_string()),
        secret: field(&pairs, "secret").filter(|s| !s.is_empty()).map(str::to_string),
        events: Some(events),
        active: Some(field(&pairs, "active").is_some()),
    };
    match crate::collab_api::patch_hook(&state, &meta, &user, id, &patch).await {
        Ok(_) => Redirect::to(&format!("/{org}/{repo}/settings/hooks?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/settings/hooks/{id}/delete
pub async fn hook_delete(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/hooks").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let result = async {
        let hook = state.store.get_webhook(meta.id, id).await?;
        state.store.delete_webhook(meta.id, id).await?;
        Ok::<_, rithub_core::CoreError>(hook)
    }
    .await;
    match result {
        Ok(hook) => {
            crate::collab_svc::enqueue_audit(&state, Some(meta.id), Some(&user), "webhook.deleted", Some(&hook.url), None);
            Redirect::to(&format!("/{org}/{repo}/settings/hooks?notice=deleted")).into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

// --- deploy keys -------------------------------------------------------------------

/// GET /{org}/{repo}/settings/keys
pub async fn keys_page(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Query(q): Query<NoticeQuery>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, _) = match admin_repo(&state, &session, &org, &repo, "/keys").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let keys = state.store.list_deploy_keys(meta.id).await.unwrap_or_default();
    let inner = keys_inner(&org, &repo, &keys, None);
    let body = shell(&org, &repo, &meta, "keys", q.notice.as_deref(), &inner);
    page("Deploy keys", &session, &body).into_response()
}

fn keys_inner(org: &str, repo: &str, keys: &[rithub_core::DeployKey], fresh_token: Option<&str>) -> String {
    let (o, r) = (esc(org), esc(repo));
    let fresh = fresh_token
        .map(|t| {
            format!(
                "<div class=\"notice ok-notice\"><strong>Deploy key added.</strong> Its HTTP token is shown once \
                 (use it as the git password over HTTPS):<div class=\"secret mono\" style=\"margin-top:6px\">{}</div></div>",
                esc(t)
            )
        })
        .unwrap_or_default();
    let mut rows = String::new();
    for k in keys {
        rows.push_str(&format!(
            "<tr><td><strong>{t}</strong><br><span class=\"mono muted\">{fp}</span></td>\
             <td><span class=\"pill\">{mode}</span></td><td class=\"muted\">{d}</td>\
             <td><form method=\"post\" action=\"/{o}/{r}/settings/keys/{id}/delete\" class=\"inline-form\">\
             <button class=\"btn small danger\" type=\"submit\">Delete</button></form></td></tr>",
            t = esc(&k.title),
            fp = esc(&super::user_settings::ssh_fingerprint(&k.key_blob)),
            mode = if k.read_only { "read-only" } else { "read/write" },
            d = short_date(k.created_at.unix_timestamp()),
            id = k.id,
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td class=\"muted\">No deploy keys. Deploy keys give a single machine access to this repository only.</td></tr>".into();
    }
    format!(
        "{fresh}<h3>Deploy keys</h3><table>{rows}</table><h3>Add deploy key</h3>\
         <form method=\"post\" action=\"/{o}/{r}/settings/keys\">\
         <label for=\"dk-title\">Title</label><input id=\"dk-title\" name=\"title\" maxlength=\"100\" placeholder=\"ci server\">\
         <label for=\"dk-key\">Public key</label><textarea id=\"dk-key\" name=\"key\" rows=\"3\" required placeholder=\"ssh-ed25519 AAAA...\"></textarea>\
         <label class=\"check\"><input type=\"checkbox\" name=\"allow_write\" value=\"1\"> Allow write access (push)</label>\
         <p style=\"margin-top:10px\"><button class=\"btn primary\" type=\"submit\">Add key</button></p></form>"
    )
}

#[derive(Deserialize)]
pub struct DeployKeyForm {
    #[serde(default)]
    title: String,
    key: String,
    #[serde(default)]
    allow_write: Option<String>,
}

/// POST /{org}/{repo}/settings/keys — shows the one-time HTTP token.
pub async fn deploy_key_create(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<DeployKeyForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/keys").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let input = crate::api::NewDeployKey {
        key: form.key.trim().to_string(),
        title: form.title,
        read_only: form.allow_write.is_none(),
    };
    match crate::api::add_deploy_key(&state, &org, &meta, &user, &input).await {
        Ok(created) => {
            let keys = state.store.list_deploy_keys(meta.id).await.unwrap_or_default();
            let inner = keys_inner(&org, &repo, &keys, Some(&created.token));
            let body = shell(&org, &repo, &meta, "keys", None, &inner);
            page("Deploy keys", &session, &body).into_response()
        }
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/{repo}/settings/keys/{id}/delete
pub async fn deploy_key_delete(
    State(state): State<AppState>,
    Path((org, repo, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (meta, user) = match admin_repo(&state, &session, &org, &repo, "/keys").await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match state.store.delete_deploy_key(meta.id, id).await {
        Ok(()) => {
            crate::audit::record(&state, Some(meta.id), Some(user.id), &user.username, "deploy_key.deleted", Some(&id.to_string()), None)
                .await;
            Redirect::to(&format!("/{org}/{repo}/settings/keys?notice=deleted")).into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

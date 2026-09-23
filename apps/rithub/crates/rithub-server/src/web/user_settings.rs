//! The signed-in user's own settings: profile (orgs, linked SSO identities),
//! personal access tokens (create with scopes + expiry, shown once; revoke),
//! and SSH keys (add with validation + fingerprint; delete).

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{DirectoryStore, OauthStore, Store, User};
use serde::Deserialize;
use uuid::Uuid;

use super::account::NoticeQuery;
use super::{
    app_error_page, error_page, esc, login_redirect, notice_html, page, session_user, short_date,
};
use crate::authz;
use crate::state::AppState;

// --- user settings ----------------------------------------------------------

fn settings_nav(active: &str) -> String {
    let item = |key: &str, label: &str| {
        let cls = if key == active { " class=\"active\"" } else { "" };
        format!("<a href=\"/settings/{key}\"{cls}>{label}</a>")
    };
    format!(
        "<nav class=\"subnav\">{}{}{}</nav>",
        item("profile", "Profile"),
        item("tokens", "Access tokens"),
        item("keys", "SSH keys")
    )
}

/// GET /settings
pub async fn settings_redirect() -> Response {
    Redirect::to("/settings/profile").into_response()
}

/// GET /settings/profile
pub async fn profile_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/profile");
    };
    let orgs = state.store.list_user_orgs(user.id).await.unwrap_or_default();
    let identities = state.store.list_oauth_identities(user.id).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>Settings</h1>{nav}\
         <table><tr><td>Username</td><td><strong>{u}</strong></td></tr>\
         <tr><td>Email</td><td>{e}</td></tr>\
         <tr><td>Member since</td><td>{since}</td></tr>\
         <tr><td>Site administrator</td><td>{admin}</td></tr></table></div>",
        nav = settings_nav("profile"),
        u = esc(&user.username),
        e = esc(&user.email),
        since = short_date(user.created_at.unix_timestamp()),
        admin = if authz::is_site_admin(user) { "Yes" } else { "No" },
    );
    body.push_str("<div class=\"card\"><h3>Organizations</h3><table>");
    for (org, role) in &orgs {
        body.push_str(&format!(
            "<tr><td><a href=\"/{o}\">{o}</a></td><td><span class=\"pill\">{r}</span></td></tr>",
            o = esc(&org.name),
            r = role.as_str()
        ));
    }
    if orgs.is_empty() {
        body.push_str("<tr><td class=\"muted\">None</td></tr>");
    }
    body.push_str("</table></div><div class=\"card\"><h3>Linked sign-in providers</h3>");
    if identities.is_empty() {
        body.push_str("<p class=\"muted\">No single-sign-on providers linked.</p>");
    } else {
        body.push_str("<table>");
        for id in &identities {
            body.push_str(&format!(
                "<tr><td>{p}</td><td class=\"muted\">{e}</td></tr>",
                p = esc(&id.provider),
                e = esc(id.email.as_deref().unwrap_or(""))
            ));
        }
        body.push_str("</table>");
    }
    body.push_str("</div>");
    page("Settings", &session, &body).into_response()
}

fn token_form() -> String {
    "<form method=\"post\" action=\"/settings/tokens\">\
     <div class=\"grid2\"><div><label for=\"tk-name\">Token name</label>\
     <input id=\"tk-name\" name=\"name\" required maxlength=\"100\" placeholder=\"laptop git\"></div>\
     <div><label for=\"tk-exp\">Expires in</label>\
     <select id=\"tk-exp\" name=\"expires_days\"><option value=\"30\">30 days</option>\
     <option value=\"90\" selected>90 days</option><option value=\"365\">1 year</option>\
     <option value=\"0\">Never</option></select></div></div>\
     <label>Scopes</label>\
     <label class=\"check\"><input type=\"checkbox\" name=\"scope_repo_read\" value=\"1\" checked> repo:read \u{2014} clone, fetch, read API</label>\
     <label class=\"check\"><input type=\"checkbox\" name=\"scope_repo_write\" value=\"1\" checked> repo:write \u{2014} push, open/merge PRs</label>\
     <label class=\"check\"><input type=\"checkbox\" name=\"scope_user\" value=\"1\"> user \u{2014} manage your tokens and SSH keys</label>\
     <label class=\"check\"><input type=\"checkbox\" name=\"scope_admin\" value=\"1\"> admin \u{2014} full access including repo/org administration</label>\
     <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Generate token</button></p></form>"
        .to_string()
}

async fn tokens_body(state: &AppState, user: &User, fresh: Option<&str>, notice: Option<&str>) -> String {
    let tokens = state.store.list_tokens(user.id).await.unwrap_or_default();
    let fresh_html = fresh
        .map(|t| {
            format!(
                "<div class=\"notice ok-notice\"><strong>New token created.</strong> Copy it now \u{2014} it will not be shown again.\
                 <div class=\"secret mono\" style=\"margin-top:6px\">{}</div></div>",
                esc(t)
            )
        })
        .unwrap_or_default();
    let mut rows = String::new();
    for t in tokens.iter().filter(|t| !t.scopes.iter().any(|s| s == authz::SESSION_SCOPE)) {
        let status = if t.revoked_at.is_some() {
            "<span class=\"pill closed\">revoked</span>".to_string()
        } else if t.expires_at.is_some_and(|e| e <= time::OffsetDateTime::now_utc()) {
            "<span class=\"pill closed\">expired</span>".to_string()
        } else {
            "<span class=\"pill open\">active</span>".to_string()
        };
        let scopes = if t.scopes.is_empty() { "full access".to_string() } else { t.scopes.join(", ") };
        let expires = t
            .expires_at
            .map(|e| short_date(e.unix_timestamp()))
            .unwrap_or_else(|| "never".into());
        let revoke = if t.revoked_at.is_none() {
            format!(
                "<form method=\"post\" action=\"/settings/tokens/{id}/revoke\" class=\"inline-form\">\
                 <button class=\"btn small danger\" type=\"submit\">Revoke</button></form>",
                id = t.id
            )
        } else {
            String::new()
        };
        rows.push_str(&format!(
            "<tr><td><strong>{n}</strong><br><span class=\"muted\">{scopes}</span></td><td>{status}</td>\
             <td class=\"muted\">created {c}<br>expires {expires}</td><td>{revoke}</td></tr>",
            n = esc(&t.name),
            scopes = esc(&scopes),
            c = short_date(t.created_at.unix_timestamp()),
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td class=\"muted\">No personal access tokens.</td></tr>".into();
    }
    format!(
        "{notice}<div class=\"card\"><h1>Settings</h1>{nav}{fresh_html}\
         <h3>Personal access tokens</h3>\
         <p class=\"muted\">Tokens authenticate git over HTTPS (as the password) and the API \
         (<code>Authorization: Bearer &lt;token&gt;</code>).</p>\
         <table>{rows}</table></div>\
         <div class=\"card\"><h3>Generate a new token</h3>{form}</div>",
        notice = notice_html(notice),
        nav = settings_nav("tokens"),
        form = token_form(),
    )
}

/// GET /settings/tokens
pub async fn tokens_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NoticeQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/tokens");
    };
    let body = tokens_body(&state, user, None, q.notice.as_deref()).await;
    page("Access tokens", &session, &body).into_response()
}

#[derive(Deserialize)]
pub struct TokenForm {
    name: String,
    #[serde(default)]
    expires_days: i64,
    #[serde(default)]
    scope_repo_read: Option<String>,
    #[serde(default)]
    scope_repo_write: Option<String>,
    #[serde(default)]
    scope_user: Option<String>,
    #[serde(default)]
    scope_admin: Option<String>,
}

/// POST /settings/tokens — mint a token and show it once.
pub async fn token_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = session.clone() else {
        return login_redirect("/settings/tokens");
    };
    let mut scopes = Vec::new();
    if form.scope_admin.is_some() {
        scopes.push("admin".to_string());
    }
    if form.scope_repo_read.is_some() {
        scopes.push("repo:read".to_string());
    }
    if form.scope_repo_write.is_some() {
        scopes.push("repo:write".to_string());
    }
    if form.scope_user.is_some() {
        scopes.push("user".to_string());
    }
    if scopes.is_empty() {
        return error_page(&session, StatusCode::BAD_REQUEST, "select at least one scope");
    }
    let expires_at = (form.expires_days > 0)
        .then(|| time::OffsetDateTime::now_utc() + time::Duration::days(form.expires_days.min(3650)));
    match crate::api::mint_token(&state, &user, &form.name, &scopes, expires_at).await {
        Ok((token, _)) => {
            let body = tokens_body(&state, &user, Some(&token), None).await;
            page("Access tokens", &session, &body).into_response()
        }
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /settings/tokens/{id}/revoke
pub async fn token_revoke(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/tokens");
    };
    match state.store.revoke_token(user.id, id).await {
        Ok(()) => {
            crate::audit::record(&state, None, Some(user.id), &user.username, "token.revoked", Some(&id.to_string()), None)
                .await;
            Redirect::to("/settings/tokens?notice=revoked").into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

/// GET /settings/keys
pub async fn keys_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NoticeQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/keys");
    };
    let keys = state.store.list_ssh_key_records(user.id).await.unwrap_or_default();
    let mut rows = String::new();
    for k in &keys {
        let fp = ssh_fingerprint(&k.key_blob);
        rows.push_str(&format!(
            "<tr><td><strong>{t}</strong><br><span class=\"mono muted\">{fp}</span></td>\
             <td class=\"muted\">added {d}</td>\
             <td><form method=\"post\" action=\"/settings/keys/{id}/delete\" class=\"inline-form\">\
             <button class=\"btn small danger\" type=\"submit\">Delete</button></form></td></tr>",
            t = esc(&k.title),
            fp = esc(&fp),
            d = short_date(k.created_at.unix_timestamp()),
            id = k.id,
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td class=\"muted\">No SSH keys yet. Add one to push over SSH.</td></tr>".into();
    }
    let ssh_hint = super::repo::ssh_host()
        .map(|h| format!("<p class=\"muted\">Clone over SSH with <code>git clone ssh://git@{}/org/repo.git</code>.</p>", esc(&h)))
        .unwrap_or_else(|| "<p class=\"muted\">SSH transport is not enabled on this server; keys take effect once it is.</p>".into());
    let body = format!(
        "{notice}<div class=\"card\"><h1>Settings</h1>{nav}<h3>SSH keys</h3>{ssh_hint}<table>{rows}</table></div>\
         <div class=\"card\"><h3>Add an SSH key</h3>\
         <form method=\"post\" action=\"/settings/keys\">\
         <label for=\"k-title\">Title</label><input id=\"k-title\" name=\"title\" maxlength=\"100\" placeholder=\"work laptop\">\
         <label for=\"k-key\">Public key</label>\
         <textarea id=\"k-key\" name=\"key\" rows=\"4\" required placeholder=\"ssh-ed25519 AAAA... you@host\"></textarea>\
         <p class=\"hint\">Paste the contents of <code>~/.ssh/id_ed25519.pub</code>.</p>\
         <p style=\"margin-top:12px\"><button class=\"btn primary\" type=\"submit\">Add key</button></p></form></div>",
        notice = notice_html(q.notice.as_deref()),
        nav = settings_nav("keys"),
    );
    page("SSH keys", &session, &body).into_response()
}

/// SHA256 fingerprint of an SSH key blob, `SHA256:<base64>` like `ssh-keygen -l`.
pub fn ssh_fingerprint(blob_b64: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    match base64::engine::general_purpose::STANDARD.decode(blob_b64) {
        Ok(raw) => {
            let digest = Sha256::digest(&raw);
            format!(
                "SHA256:{}",
                base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
            )
        }
        Err(_) => "invalid key".to_string(),
    }
}

#[derive(Deserialize)]
pub struct KeyForm {
    #[serde(default)]
    title: String,
    key: String,
}

/// POST /settings/keys
pub async fn key_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<KeyForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/keys");
    };
    match crate::api::register_ssh_key(&state, user, form.key.trim(), Some(&form.title)).await {
        Ok(_) => Redirect::to("/settings/keys?notice=added").into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /settings/keys/{id}/delete
pub async fn key_delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/settings/keys");
    };
    match state.store.delete_ssh_key(user.id, id).await {
        Ok(()) => {
            crate::audit::record(&state, None, Some(user.id), &user.username, "ssh_key.deleted", Some(&id.to_string()), None)
                .await;
            Redirect::to("/settings/keys?notice=deleted").into_response()
        }
        Err(e) => app_error_page(&session, e.into()),
    }
}

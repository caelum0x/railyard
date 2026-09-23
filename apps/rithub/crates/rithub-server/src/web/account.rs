//! Account-level pages: the dashboard, "new repository" / "new organization",
//! org and user profile pages (with member + team management for org admins),
//! and the user's own settings (profile, access tokens, SSH keys).

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rithub_core::{
    AccessStore, DirectoryStore, NewOrganization, NewRepository, Organization, Role,
    Store, User, Visibility,
};
use serde::Deserialize;

use super::{
    app_error_page, error_page, esc, icon, login_redirect, notice_html, page, session_user,
    short_date,
};
use crate::authz;
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct NoticeQuery {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
}

fn repo_row(org: &str, repo: &rithub_core::Repository) -> String {
    let lock = if repo.visibility == Visibility::Private {
        format!(" <span class=\"pill\">{} Private</span>", icon("lock"))
    } else {
        String::new()
    };
    let desc = repo
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| format!("<br><span class=\"muted\">{}</span>", esc(d)))
        .unwrap_or_default();
    format!(
        "<tr><td>{icon} <a href=\"/{o}/{r}\"><strong>{o}/{r}</strong></a>{lock}{desc}</td>\
         <td class=\"muted\" style=\"white-space:nowrap\">{date}</td></tr>",
        icon = icon("repo"),
        o = esc(org),
        r = esc(&repo.name),
        date = short_date(repo.created_at.unix_timestamp()),
    )
}

/// GET / — dashboard for signed-in users; public repository directory otherwise.
pub async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    let orgs = match state.store.list_orgs().await {
        Ok(orgs) => orgs,
        Err(_) => return error_page(&session, StatusCode::INTERNAL_SERVER_ERROR, "store error"),
    };
    let mut body = String::new();

    if let Some(user) = &session {
        let mine = state.store.list_user_orgs(user.id).await.unwrap_or_default();
        body.push_str(&format!(
            "<div class=\"card\"><div class=\"row\"><h1>Your repositories</h1><span class=\"spacer\"></span>\
             <a class=\"btn primary\" href=\"/new\">New repository</a>\
             <a class=\"btn\" href=\"/orgs/new\">New organization</a></div>"
        ));
        let mut rows = String::new();
        for (org, _) in &mine {
            if let Ok(repos) = crate::api::visible_repos(&state, org, Some(user)).await {
                for repo in repos {
                    rows.push_str(&repo_row(&org.name, &repo));
                }
            }
        }
        if rows.is_empty() {
            body.push_str(
                "<div class=\"empty-state\"><h3>No repositories yet</h3>\
                 <p>Create your first repository, then push to it with git.</p>\
                 <p><a class=\"btn primary\" href=\"/new\">Create a repository</a></p></div>",
            );
        } else {
            body.push_str(&format!("<table>{rows}</table>"));
        }
        body.push_str("</div>");
        if !mine.is_empty() {
            body.push_str("<div class=\"card\"><h3>Your organizations</h3><div class=\"row\">");
            for (org, role) in &mine {
                body.push_str(&format!(
                    "<a class=\"btn\" href=\"/{o}\">{o} <span class=\"muted\">{role}</span></a>",
                    o = esc(&org.name),
                    role = role.as_str()
                ));
            }
            body.push_str("</div></div>");
        }
    }

    body.push_str("<div class=\"card\"><h2>Explore</h2>");
    let mut rows = String::new();
    for org in &orgs {
        if let Ok(repos) = crate::api::visible_repos(&state, org, session.as_ref()).await {
            for repo in repos.into_iter().filter(|r| r.visibility == Visibility::Public) {
                rows.push_str(&repo_row(&org.name, &repo));
            }
        }
    }
    if rows.is_empty() {
        let cta = if session.is_some() {
            "<p><a class=\"btn primary\" href=\"/new\">Create a repository</a></p>"
        } else if authz::open_signup() {
            "<p><a class=\"btn primary\" href=\"/signup\">Sign up</a> to create the first one.</p>"
        } else {
            ""
        };
        body.push_str(&format!(
            "<div class=\"empty-state\"><h3>No public repositories yet</h3>{cta}</div>"
        ));
    } else {
        body.push_str(&format!("<table>{rows}</table>"));
    }
    body.push_str("</div>");
    page("Dashboard", &session, &body).into_response()
}

// --- new repository ---------------------------------------------------------

async fn manageable_orgs(state: &AppState, user: &User) -> Vec<Organization> {
    if authz::is_site_admin(user) {
        return state.store.list_orgs().await.unwrap_or_default();
    }
    state
        .store
        .list_user_orgs(user.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, role)| role.can_manage_repos())
        .map(|(org, _)| org)
        .collect()
}

fn new_repo_body(orgs: &[Organization], selected: &str, error: Option<&str>, name: &str, desc: &str) -> String {
    let err = error
        .map(|e| format!("<div class=\"notice bad-notice\" role=\"alert\">{}</div>", esc(e)))
        .unwrap_or_default();
    let options: String = orgs
        .iter()
        .map(|o| {
            let sel = if o.name == selected { " selected" } else { "" };
            format!("<option value=\"{n}\"{sel}>{n}</option>", n = esc(&o.name))
        })
        .collect();
    format!(
        "<div class=\"card\" style=\"max-width:640px\"><h1>Create a new repository</h1>{err}\
         <form method=\"post\" action=\"/new\">\
         <div class=\"row\" style=\"align-items:flex-end\">\
         <div style=\"flex:1;min-width:160px\"><label for=\"nr-owner\">Owner</label>\
         <select id=\"nr-owner\" name=\"org\" required>{options}</select></div>\
         <span style=\"padding-bottom:8px\">/</span>\
         <div style=\"flex:2;min-width:200px\"><label for=\"nr-name\">Repository name</label>\
         <input id=\"nr-name\" name=\"name\" value=\"{name}\" required maxlength=\"100\"></div></div>\
         <label for=\"nr-desc\">Description <span class=\"muted\">(optional)</span></label>\
         <input id=\"nr-desc\" name=\"description\" value=\"{desc}\" maxlength=\"350\">\
         <label for=\"nr-branch\">Default branch</label>\
         <input id=\"nr-branch\" name=\"default_branch\" value=\"main\" required maxlength=\"100\">\
         <label>Visibility</label>\
         <label class=\"check\"><input type=\"radio\" name=\"visibility\" value=\"public\" checked> Public \u{2014} anyone can see and clone</label>\
         <label class=\"check\"><input type=\"radio\" name=\"visibility\" value=\"private\"> {lock} Private \u{2014} only people you grant access</label>\
         <p style=\"margin-top:14px\"><button class=\"btn primary\" type=\"submit\">Create repository</button></p>\
         </form></div>",
        name = esc(name),
        desc = esc(desc),
        lock = icon("lock"),
    )
}

/// GET /new
pub async fn new_repo_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NoticeQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = &session else {
        return login_redirect("/new");
    };
    let orgs = manageable_orgs(&state, user).await;
    if orgs.is_empty() {
        let body = "<div class=\"card\"><h1>Create a new repository</h1>\
                    <p class=\"muted\">You are not an admin of any organization yet.</p>\
                    <p><a class=\"btn primary\" href=\"/orgs/new\">Create an organization</a></p></div>";
        return page("New repository", &session, body).into_response();
    }
    let selected = q.org.clone().unwrap_or_else(|| user.username.clone());
    page("New repository", &session, &new_repo_body(&orgs, &selected, None, "", "")).into_response()
}

#[derive(Deserialize)]
pub struct NewRepoForm {
    org: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    visibility: String,
}

/// POST /new
pub async fn new_repo_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewRepoForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = session.clone() else {
        return login_redirect("/new");
    };
    let orgs = manageable_orgs(&state, &user).await;
    let Some(org) = orgs.iter().find(|o| o.name == form.org).cloned() else {
        return error_page(&session, StatusCode::FORBIDDEN, "you cannot create repositories in that organization");
    };
    let input = NewRepository {
        name: form.name.trim().to_string(),
        description: Some(form.description.trim().to_string()).filter(|d| !d.is_empty()),
        default_branch: Some(form.default_branch.trim().to_string()).filter(|b| !b.is_empty()),
        visibility: Some(if form.visibility == "private" {
            Visibility::Private
        } else {
            Visibility::Public
        }),
    };
    match crate::api::create_repo_in(&state, &org, &user, &input).await {
        Ok(repo) => Redirect::to(&format!("/{}/{}", org.name, repo.name)).into_response(),
        Err(e) => {
            let msg = if e.status() == StatusCode::CONFLICT {
                "A repository with that name already exists in this organization.".to_string()
            } else {
                e.client_message()
            };
            (
                e.status(),
                page(
                    "New repository",
                    &session,
                    &new_repo_body(&orgs, &form.org, Some(&msg), &form.name, &form.description),
                ),
            )
                .into_response()
        }
    }
}

// --- new organization -------------------------------------------------------

fn new_org_body(error: Option<&str>, name: &str, display: &str) -> String {
    let err = error
        .map(|e| format!("<div class=\"notice bad-notice\" role=\"alert\">{}</div>", esc(e)))
        .unwrap_or_default();
    format!(
        "<div class=\"card\" style=\"max-width:560px\"><h1>Create an organization</h1>{err}\
         <form method=\"post\" action=\"/orgs/new\">\
         <label for=\"no-name\">Organization name</label>\
         <input id=\"no-name\" name=\"name\" value=\"{n}\" required maxlength=\"100\">\
         <p class=\"hint\">Used in URLs: <code>/name/repo</code>. You become its owner.</p>\
         <label for=\"no-display\">Display name <span class=\"muted\">(optional)</span></label>\
         <input id=\"no-display\" name=\"display_name\" value=\"{d}\" maxlength=\"100\">\
         <p style=\"margin-top:14px\"><button class=\"btn primary\" type=\"submit\">Create organization</button></p>\
         </form></div>",
        n = esc(name),
        d = esc(display),
    )
}

/// GET /orgs/new
pub async fn new_org_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = session_user(&state, &headers).await;
    if session.is_none() {
        return login_redirect("/orgs/new");
    }
    page("New organization", &session, &new_org_body(None, "", "")).into_response()
}

#[derive(Deserialize)]
pub struct NewOrgForm {
    name: String,
    #[serde(default)]
    display_name: String,
}

/// POST /orgs/new
pub async fn new_org_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewOrgForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let Some(user) = session.clone() else {
        return login_redirect("/orgs/new");
    };
    let input = NewOrganization {
        name: form.name.trim().to_string(),
        display_name: Some(form.display_name.trim().to_string()).filter(|d| !d.is_empty()),
    };
    match crate::api::create_org_for(&state, &user, &input).await {
        Ok(org) => Redirect::to(&format!("/{}?notice=created", org.name)).into_response(),
        Err(e) => {
            let msg = if e.status() == StatusCode::CONFLICT {
                "That name is already taken.".to_string()
            } else {
                e.client_message()
            };
            (
                e.status(),
                page("New organization", &session, &new_org_body(Some(&msg), &form.name, &form.display_name)),
            )
                .into_response()
        }
    }
}

// --- org / user pages -------------------------------------------------------

/// GET /{owner} — an organization page, or a user profile.
pub async fn owner_page(
    State(state): State<AppState>,
    Path(owner): Path<String>,
    headers: HeaderMap,
    Query(q): Query<NoticeQuery>,
) -> Response {
    let session = session_user(&state, &headers).await;
    match state.store.get_org(&owner).await {
        Ok(org) => org_page(&state, &session, org, q.notice.as_deref()).await,
        Err(_) => match state.store.get_user(&owner).await {
            Ok(user) => user_page(&state, &session, user).await,
            Err(_) => error_page(&session, StatusCode::NOT_FOUND, "no such user or organization"),
        },
    }
}

async fn org_page(state: &AppState, session: &Option<User>, org: Organization, notice: Option<&str>) -> Response {
    let viewer_role = match session {
        Some(u) => authz::user_org_role(state, u, &org).await,
        None => None,
    };
    let repos = crate::api::visible_repos(state, &org, session.as_ref())
        .await
        .unwrap_or_default();
    let title = org.display_name.clone().unwrap_or_else(|| org.name.clone());
    let new_btn = if viewer_role.is_some_and(|r| r.can_manage_repos()) {
        format!(
            "<a class=\"btn primary\" href=\"/new?org={}\">New repository</a>",
            super::url_encode(&org.name)
        )
    } else {
        String::new()
    };
    let mut body = format!(
        "{notice}<div class=\"card\"><div class=\"row\"><h1>{title} <span class=\"muted\">@{name}</span></h1>\
         <span class=\"spacer\"></span>{new_btn}</div>\
         <p class=\"muted\">Organization since {since}</p></div>",
        notice = notice_html(notice),
        title = esc(&title),
        name = esc(&org.name),
        since = short_date(org.created_at.unix_timestamp()),
    );
    body.push_str("<div class=\"card\"><h3>Repositories</h3>");
    if repos.is_empty() {
        body.push_str("<p class=\"muted\">No repositories visible to you.</p>");
    } else {
        body.push_str("<table>");
        for repo in &repos {
            body.push_str(&repo_row(&org.name, repo));
        }
        body.push_str("</table>");
    }
    body.push_str("</div>");

    // Members + teams are visible to members; management to admins/owners.
    if let Some(role) = viewer_role {
        body.push_str(&members_card(state, &org, role).await);
        body.push_str(&teams_card(state, &org, role, &repos).await);
        body.push_str(&billing_card(state, &org).await);
    }
    page(&org.name, session, &body).into_response()
}

async fn members_card(state: &AppState, org: &Organization, role: Role) -> String {
    let members = state.store.list_org_members(org.id).await.unwrap_or_default();
    let o = esc(&org.name);
    let mut out = format!("<div class=\"card\"><h3>Members ({})</h3><table>", members.len());
    for (user, member_role) in &members {
        let remove = if role == Role::Owner {
            format!(
                "<form method=\"post\" action=\"/{o}/-/members/{u}/remove\" class=\"inline-form\">\
                 <button class=\"btn small\" type=\"submit\">Remove</button></form>",
                u = esc(&user.username)
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<tr><td><a href=\"/{u}\">{u}</a></td><td><span class=\"pill\">{r}</span></td><td>{remove}</td></tr>",
            u = esc(&user.username),
            r = member_role.as_str()
        ));
    }
    out.push_str("</table>");
    if role == Role::Owner {
        out.push_str(&format!(
            "<form method=\"post\" action=\"/{o}/-/members\" class=\"row\" style=\"margin-top:10px\">\
             <input name=\"username\" placeholder=\"username\" required style=\"flex:2;min-width:140px\" aria-label=\"Username\">\
             <select name=\"role\" style=\"flex:1;min-width:110px\" aria-label=\"Role\">\
             <option value=\"member\">member</option><option value=\"admin\">admin</option><option value=\"owner\">owner</option></select>\
             <button class=\"btn\" type=\"submit\">Add or update member</button></form>"
        ));
    }
    out.push_str("</div>");
    out
}

async fn teams_card(state: &AppState, org: &Organization, role: Role, repos: &[rithub_core::Repository]) -> String {
    let teams = state.store.list_teams(org.id).await.unwrap_or_default();
    let o = esc(&org.name);
    let mut out = String::from("<div class=\"card\"><h3>Teams</h3>");
    if teams.is_empty() {
        out.push_str("<p class=\"muted\">No teams yet. Teams grant a group of members access to repositories.</p>");
    }
    let repo_options: String = repos
        .iter()
        .map(|r| format!("<option value=\"{n}\">{n}</option>", n = esc(&r.name)))
        .collect();
    for team in &teams {
        let t = esc(&team.name);
        out.push_str(&format!("<div class=\"comment\"><div class=\"comment-h\"><strong>{t}</strong></div><div class=\"comment-b\">"));
        if role >= Role::Admin {
            out.push_str(&format!(
                "<form method=\"post\" action=\"/{o}/-/teams/{t}/members\" class=\"row\">\
                 <input name=\"username\" placeholder=\"add member by username\" required style=\"flex:1\" aria-label=\"Username\">\
                 <button class=\"btn small\" type=\"submit\">Add member</button></form>\
                 <form method=\"post\" action=\"/{o}/-/teams/{t}/repos\" class=\"row\" style=\"margin-top:6px\">\
                 <select name=\"repo\" style=\"flex:2\" aria-label=\"Repository\">{repo_options}</select>\
                 <select name=\"role\" style=\"flex:1\" aria-label=\"Role\"><option>read</option><option>write</option><option>admin</option></select>\
                 <button class=\"btn small\" type=\"submit\">Grant repository access</button></form>"
            ));
        } else {
            out.push_str("<p class=\"muted\">Managed by organization admins.</p>");
        }
        out.push_str("</div></div>");
    }
    if role >= Role::Admin {
        out.push_str(&format!(
            "<form method=\"post\" action=\"/{o}/-/teams\" class=\"row\" style=\"margin-top:10px\">\
             <input name=\"name\" placeholder=\"new team name\" required style=\"flex:1\" aria-label=\"Team name\">\
             <button class=\"btn\" type=\"submit\">Create team</button></form>"
        ));
    }
    out.push_str("</div>");
    out
}

async fn billing_card(state: &AppState, org: &Organization) -> String {
    let private = state.store.count_private_repos(org.id).await.unwrap_or(0);
    let seats = state.store.count_org_members(org.id).await.unwrap_or(0);
    let plan = match crate::billing::mode() {
        crate::billing::BillingMode::Disabled => "Unlimited (billing not configured on this server)".to_string(),
        crate::billing::BillingMode::LocalCap(cap) => format!("Free tier: up to {cap} private repositories"),
        crate::billing::BillingMode::Misconfigured => "Billing misconfigured (RAGO_API_KEY missing) \u{2014} private repo creation is blocked".to_string(),
        crate::billing::BillingMode::Rago => match crate::billing::private_repo_limit(org).await {
            Ok(Some(limit)) => format!("Plan limit: {limit} private repositories"),
            Ok(None) => "Plan: unlimited private repositories".to_string(),
            Err(e) => format!("Plan status unavailable: {}", e.client_message()),
        },
    };
    format!(
        "<div class=\"card\"><h3>Plan &amp; usage</h3><table>\
         <tr><td>Plan</td><td>{plan}</td></tr>\
         <tr><td>Private repositories</td><td>{private}</td></tr>\
         <tr><td>Seats (members)</td><td>{seats}</td></tr></table></div>",
        plan = esc(&plan),
    )
}

async fn user_page(state: &AppState, session: &Option<User>, user: User) -> Response {
    let orgs = state.store.list_user_orgs(user.id).await.unwrap_or_default();
    let mut body = format!(
        "<div class=\"card\"><h1>{u}</h1><p class=\"muted\">Member since {since}</p></div>",
        u = esc(&user.username),
        since = short_date(user.created_at.unix_timestamp()),
    );
    body.push_str("<div class=\"card\"><h3>Organizations</h3>");
    if orgs.is_empty() {
        body.push_str("<p class=\"muted\">Not a member of any organization.</p>");
    } else {
        body.push_str("<div class=\"row\">");
        for (org, _) in &orgs {
            body.push_str(&format!("<a class=\"btn\" href=\"/{o}\">{o}</a>", o = esc(&org.name)));
        }
        body.push_str("</div>");
    }
    body.push_str("</div>");
    page(&user.username, session, &body).into_response()
}

// --- org management (POST) ---------------------------------------------------

async fn org_admin(
    state: &AppState,
    session: &Option<User>,
    org: &str,
    needed: Role,
) -> Result<(Organization, User), Response> {
    let Some(user) = session.clone() else {
        return Err(login_redirect(&format!("/{org}")));
    };
    let organization = state
        .store
        .get_org(org)
        .await
        .map_err(|_| error_page(session, StatusCode::NOT_FOUND, "organization not found"))?;
    match authz::user_org_role(state, &user, &organization).await {
        Some(r) if r >= needed => Ok((organization, user)),
        _ => Err(error_page(
            session,
            StatusCode::FORBIDDEN,
            &format!("requires {} role in {org}", needed.as_str()),
        )),
    }
}

#[derive(Deserialize)]
pub struct MemberForm {
    username: String,
    #[serde(default)]
    role: String,
}

/// POST /{org}/-/members (owners)
pub async fn member_add(
    State(state): State<AppState>,
    Path(org): Path<String>,
    headers: HeaderMap,
    Form(form): Form<MemberForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (organization, actor) = match org_admin(&state, &session, &org, Role::Owner).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let role = Role::parse(&form.role);
    match crate::api::set_member_role(&state, &organization, &actor, &form.username, role).await {
        Ok(_) => Redirect::to(&format!("/{org}?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

/// POST /{org}/-/members/{user}/remove (owners)
pub async fn member_remove(
    State(state): State<AppState>,
    Path((org, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (organization, actor) = match org_admin(&state, &session, &org, Role::Owner).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::api::remove_member(&state, &organization, &actor, &username).await {
        Ok(()) => Redirect::to(&format!("/{org}?notice=removed")).into_response(),
        Err(e) => app_error_page(&session, e),
    }
}

#[derive(Deserialize)]
pub struct TeamForm {
    name: String,
}

/// POST /{org}/-/teams (admins)
pub async fn team_create(
    State(state): State<AppState>,
    Path(org): Path<String>,
    headers: HeaderMap,
    Form(form): Form<TeamForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (organization, _) = match org_admin(&state, &session, &org, Role::Admin).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(e) = rithub_core::validate_name("team", form.name.trim()) {
        return app_error_page(&session, e.into());
    }
    match state.store.create_team(organization.id, form.name.trim()).await {
        Ok(_) => Redirect::to(&format!("/{org}?notice=created")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

#[derive(Deserialize)]
pub struct TeamMemberForm {
    username: String,
}

/// POST /{org}/-/teams/{team}/members (admins)
pub async fn team_member_add(
    State(state): State<AppState>,
    Path((org, team)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<TeamMemberForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (organization, _) = match org_admin(&state, &session, &org, Role::Admin).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let result = async {
        let team = state.store.get_team(organization.id, &team).await?;
        let user = state.store.get_user(form.username.trim()).await?;
        state.store.add_team_member(team.id, user.id).await?;
        Ok::<(), rithub_core::CoreError>(())
    }
    .await;
    match result {
        Ok(()) => Redirect::to(&format!("/{org}?notice=added")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

#[derive(Deserialize)]
pub struct TeamRepoForm {
    repo: String,
    role: String,
}

/// POST /{org}/-/teams/{team}/repos (admins)
pub async fn team_repo_grant(
    State(state): State<AppState>,
    Path((org, team)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<TeamRepoForm>,
) -> Response {
    let session = session_user(&state, &headers).await;
    let (organization, _) = match org_admin(&state, &session, &org, Role::Admin).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let role = match crate::governance_api::parse_repo_role(&form.role) {
        Ok(r) => r,
        Err(e) => return app_error_page(&session, e),
    };
    let result = async {
        let team = state.store.get_team(organization.id, &team).await?;
        let repo = state.store.get_repo(&org, &form.repo).await?;
        state.store.grant_team_repo(team.id, repo.id, role).await?;
        Ok::<(), rithub_core::CoreError>(())
    }
    .await;
    match result {
        Ok(()) => Redirect::to(&format!("/{org}?notice=saved")).into_response(),
        Err(e) => app_error_page(&session, e.into()),
    }
}

//! rithub-server as a library: the router and every module, so the binary
//! and the integration tests (a live-Postgres harness) build the same app.

pub mod accounts_api;
pub mod api;
pub mod audit;
pub mod auth;
pub mod authz;
pub mod billing;
pub mod branches;
pub mod ci;
pub mod collab_api;
pub mod collab_svc;
pub mod pulls_svc;
pub mod error;
pub mod git_http;
pub mod governance_api;
pub mod graphql;
pub mod jobs;
pub mod jobs_api;
pub mod milestones_api;
pub mod oauth;
pub mod releases_api;
pub mod search_api;
pub mod search_index;
pub mod ssh;
pub mod state;
pub mod web;
pub mod webhooks;
pub mod lfs;

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Max git request body (a pushed pack).
pub const MAX_GIT_BODY_BYTES: usize = 1024 * 1024 * 1024;

/// The full HTTP application over a ready [`AppState`].
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/orgs", post(api::create_org).get(api::list_orgs))
        .route("/api/orgs/{org}", get(api::get_org))
        .route(
            "/api/orgs/{org}/repos",
            post(api::create_repo).get(api::list_repos),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}",
            get(api::get_repo)
                .patch(api::update_repo)
                .delete(api::delete_repo),
        )
        .route("/api/orgs/{org}/repos/{repo}/seed", post(api::seed_repo))
        .route("/api/orgs/{org}/repos/{repo}/forks", post(api::create_fork))
        .route(
            "/api/orgs/{org}/members",
            post(api::add_member).get(api::list_members),
        )
        .route(
            "/api/orgs/{org}/members/{username}",
            delete(api::delete_member),
        )
        .route("/api/users", post(api::create_user))
        .route("/api/user", get(api::current_user))
        .route(
            "/api/users/{username}/keys",
            post(api::add_ssh_key).get(api::list_ssh_keys),
        )
        .route(
            "/api/users/{username}/keys/{key_id}",
            delete(api::delete_ssh_key),
        )
        .route("/api/tokens", post(api::create_token))
        .route(
            "/api/users/{username}/tokens",
            get(api::list_tokens),
        )
        .route(
            "/api/users/{username}/tokens/{token_id}",
            delete(api::revoke_token),
        )
        // --- deploy keys ---
        .route(
            "/api/orgs/{org}/repos/{repo}/keys",
            post(api::create_deploy_key).get(api::list_deploy_keys),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/keys/{key_id}",
            delete(api::delete_deploy_key),
        )
        // --- collaboration: issues ---
        .route(
            "/api/orgs/{org}/repos/{repo}/issues",
            post(collab_api::create_issue).get(collab_api::list_issues),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}",
            get(collab_api::get_issue).patch(collab_api::update_issue),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}/close",
            post(collab_api::close_issue),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}/reopen",
            post(collab_api::reopen_issue),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}/comments",
            post(collab_api::comment_issue).get(collab_api::list_issue_comments),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/labels",
            get(collab_api::list_labels).post(collab_api::create_label),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/labels/{name}",
            axum::routing::patch(collab_api::update_label).delete(collab_api::delete_label),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}/labels",
            post(collab_api::label_issue),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}/assignees",
            post(collab_api::assign_issue),
        )
        // --- collaboration: pull requests ---
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls",
            post(collab_api::create_pr).get(collab_api::list_prs),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}",
            get(collab_api::get_pr).patch(collab_api::update_pr),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/diff",
            get(collab_api::pr_diff),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/commits",
            get(collab_api::pr_commits),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/mergeability",
            get(collab_api::pr_mergeability),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/comments",
            post(collab_api::comment_pr).get(collab_api::list_pr_comments),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/close",
            post(collab_api::close_pr),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/reopen",
            post(collab_api::reopen_pr),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/merge",
            post(collab_api::merge_pr),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/reviews",
            post(collab_api::create_review).get(collab_api::list_reviews),
        )
        // --- protected branches ---
        .route(
            "/api/orgs/{org}/repos/{repo}/branches/{branch}/protection",
            post(collab_api::set_protection)
                .get(collab_api::get_protection)
                .delete(branches::api_delete_protection),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/branches",
            get(branches::api_list_branches),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/branches/{branch}",
            axum::routing::delete(branches::api_delete_branch),
        )
        // --- webhooks ---
        .route(
            "/api/orgs/{org}/repos/{repo}/hooks",
            post(collab_api::create_hook).get(collab_api::list_hooks),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/hooks/{hook_id}",
            get(collab_api::get_hook)
                .patch(collab_api::update_hook)
                .delete(collab_api::delete_hook),
        )
        // --- milestones ---
        .route(
            "/api/orgs/{org}/repos/{repo}/milestones",
            post(milestones_api::create_milestone).get(milestones_api::list_milestones),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/milestones/{number}",
            get(milestones_api::get_milestone)
                .patch(milestones_api::update_milestone)
                .delete(milestones_api::delete_milestone),
        )
        // --- releases (JSON API) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/releases",
            post(releases_api::create_release).get(releases_api::list_releases),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/releases/{tag}",
            axum::routing::patch(releases_api::update_release)
                .delete(releases_api::delete_release),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/releases/{tag}/archive",
            get(releases_api::download_archive),
        )
        // --- release assets (binary uploads) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/releases/{tag}/assets",
            post(releases_api::upload_asset).get(releases_api::list_assets),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/releases/{tag}/assets/{asset_id}",
            get(releases_api::download_asset).delete(releases_api::delete_asset),
        )
        // --- CI runners (external-runner registration) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/runners",
            post(ci::register_runner).get(ci::list_runners),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/runners/{id}",
            delete(ci::delete_runner),
        )
        // --- check runs (runner report API + surfacing) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/check-runs",
            post(ci::create_check_run),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/check-runs/{id}",
            axum::routing::patch(ci::update_check_run),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/check-runs/{id}/rerun",
            post(ci::api_rerun_check_run),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/check-runs/{id}/cancel",
            post(ci::api_cancel_check_run),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/commits/{sha}/check-runs",
            get(ci::list_commit_check_runs),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/check-runs",
            get(ci::list_pr_check_runs),
        )
        // --- CI statuses ---
        .route(
            "/api/orgs/{org}/repos/{repo}/commits/{sha}/statuses",
            post(governance_api::set_status),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/commits/{sha}/status",
            get(governance_api::get_status),
        )
        // --- audit + search ---
        .route(
            "/api/orgs/{org}/repos/{repo}/audit",
            get(governance_api::list_audit),
        )
        // Full-text search (Tantivy or Postgres backend), permission-scoped.
        .route("/api/search", get(search_api::search))
        .route(
            "/api/users/{username}/notifications",
            get(governance_api::list_notifications),
        )
        .route(
            "/api/users/{username}/notifications/read",
            post(governance_api::mark_all_notifications_read),
        )
        .route(
            "/api/users/{username}/notifications/{id}/read",
            post(governance_api::mark_notification_read),
        )
        // --- collaborators + teams (fine-grained RBAC) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/collaborators",
            post(governance_api::add_collaborator).get(governance_api::list_collaborators),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/collaborators/{username}",
            delete(governance_api::remove_collaborator),
        )
        .route(
            "/api/orgs/{org}/teams",
            post(governance_api::create_team).get(governance_api::list_teams),
        )
        .route(
            "/api/orgs/{org}/teams/{team}/members",
            post(governance_api::add_team_member),
        )
        .route(
            "/api/orgs/{org}/teams/{team}/repos",
            post(governance_api::grant_team_repo),
        )
        .route(
            "/api/orgs/{org}/teams/{team}",
            get(governance_api::get_team).delete(governance_api::delete_team),
        )
        .route(
            "/api/orgs/{org}/teams/{team}/members/{username}",
            delete(governance_api::remove_team_member),
        )
        .route(
            "/api/orgs/{org}/teams/{team}/repos/{repo}",
            delete(governance_api::revoke_team_repo),
        )
        // --- admin: job outbox ---
        .route("/api/admin/jobs", get(jobs_api::list_jobs))
        .route(
            "/api/admin/jobs/{id}/redeliver",
            post(jobs_api::redeliver_job),
        )
        // --- admin: search index reindex ---
        .route("/api/admin/search/reindex", post(search_api::reindex))
        // --- GraphQL API (Query + Mutation), plus SDL export + dev playground ---
        .route(
            "/graphql",
            get(graphql::graphiql).post(graphql::graphql_handler),
        )
        .route("/graphql/schema", get(graphql::graphql_schema))
        // Git smart + dumb HTTP: one catch-all per method.
        .route(
            "/{owner}/{repo}/{*path}",
            get(git_http::git_get).post(git_http::git_post).put(git_http::git_put),
        )
        // OAuth2 / SSO login (authorization-code flow: start + callback).
        .merge(oauth::routes())
        // Server-rendered web UI (landing, repo browse, issues, PRs, releases).
        .merge(web::routes())
        .layer(DefaultBodyLimit::max(MAX_GIT_BODY_BYTES))
        .layer(middleware::from_fn(same_origin_guard))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// CSRF defense for every state-changing request: when a browser sends an
/// `Origin` (all modern browsers do on cross-origin POST/PATCH/DELETE) it must
/// match the request's `Host`, and `Sec-Fetch-Site: cross-site` is refused.
/// Non-browser clients (git, curl, SDKs) send neither header and pass through;
/// they authenticate with explicit credentials, not ambient cookies.
async fn same_origin_guard(request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        let headers = request.headers();
        if headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "cross-site")
        {
            return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
        }
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let host = headers
                .get("x-forwarded-host")
                .or_else(|| headers.get(header::HOST))
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let origin_host = origin
                .split_once("://")
                .map(|(_, rest)| rest)
                .unwrap_or(origin)
                .trim_end_matches('/');
            let public_host = std::env::var("RITHUB_PUBLIC_URL")
                .ok()
                .and_then(|u| u.split_once("://").map(|(_, rest)| rest.trim_end_matches('/').to_string()))
                .unwrap_or_default();
            if origin == "null" || (origin_host != host && origin_host != public_host) {
                return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
            }
        }
    }
    next.run(request).await
}

/// Baseline security headers on every response.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(header::HeaderValue::from_static("nosniff"));
    headers
        .entry(header::X_FRAME_OPTIONS)
        .or_insert(header::HeaderValue::from_static("DENY"));
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(header::HeaderValue::from_static("same-origin"));
    response
}

async fn healthz() -> &'static str {
    "ok"
}


//! # rithub-server
//!
//! The axum HTTP service that makes rithub a real, self-hostable git forge:
//!
//! - a **forge JSON API** (`/api/...`) for organizations and repositories,
//!   backed by Postgres via `rithub-core`;
//! - a **dumb-HTTP git server** (`/{owner}/{repo}.git/...`) that hosts the
//!   `rit`-backed bare repositories, so a stock `git clone`/`git fetch` works.
//!
//! Configuration is via environment (fail-closed on missing required values):
//!
//! | Var | Meaning | Default |
//! |-----|---------|---------|
//! | `DATABASE_URL` | Postgres connection string | *required* |
//! | `RITHUB_DATA_ROOT` | directory that holds bare repos | `./rithub-data` |
//! | `RITHUB_BIND` | listen address | `127.0.0.1:8080` |

mod api;
mod audit;
mod auth;
mod collab_api;
mod error;
mod git_http;
mod governance_api;
mod releases_api;
mod ssh;
mod state;
mod web;
mod webhooks;

use std::path::PathBuf;

use anyhow::Context;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use rithub_core::PgStore;
use rithub_git::RepoHost;
use sqlx::postgres::PgPoolOptions;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Max git request body (a pushed pack). Matches `rit`'s receive-pack pack cap
/// so the transport buffer and the receiver agree; `rit` enforces the real limit.
const MAX_GIT_BODY_BYTES: usize = 1024 * 1024 * 1024;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must be set (Postgres connection string)")?;
    let data_root: PathBuf = std::env::var("RITHUB_DATA_ROOT")
        .unwrap_or_else(|_| "./rithub-data".to_string())
        .into();
    let bind = std::env::var("RITHUB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    std::fs::create_dir_all(&data_root)
        .with_context(|| format!("creating data root {}", data_root.display()))?;

    // Connect and bring the schema up to date (embedded migrations).
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .context("connecting to Postgres")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    let state = AppState::new(PgStore::new(pool), RepoHost::new(&data_root));

    // Optional SSH transport: enabled when RITHUB_SSH_BIND is set. It serves the
    // same repos with the same RBAC + protected-branch enforcement as HTTP.
    if let Ok(ssh_bind) = std::env::var("RITHUB_SSH_BIND") {
        let rit_bin = std::env::var("RITHUB_RIT_BIN").unwrap_or_else(|_| "rit".to_string());
        let ssh_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = ssh::serve(ssh_state, ssh_bind, rit_bin).await {
                tracing::error!(%error, "SSH transport stopped");
            }
        });
    }

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, data_root = %data_root.display(), "rithub-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

/// Assemble the router. The forge API is namespaced under `/api`; the dumb git
/// protocol is served by a catch-all under `/{owner}/{repo}/...`. axum's router
/// gives static segments (`/api`, `/healthz`) precedence over the catch-all, so
/// the two coexist without ambiguity.
fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/orgs", post(api::create_org).get(api::list_orgs))
        .route("/api/orgs/{org}", get(api::get_org))
        .route(
            "/api/orgs/{org}/repos",
            post(api::create_repo).get(api::list_repos),
        )
        .route("/api/orgs/{org}/repos/{repo}", get(api::get_repo))
        .route("/api/orgs/{org}/repos/{repo}/seed", post(api::seed_repo))
        .route("/api/orgs/{org}/repos/{repo}/forks", post(api::create_fork))
        .route("/api/orgs/{org}/members", post(api::add_member))
        .route("/api/users", post(api::create_user))
        .route("/api/users/{username}/keys", post(api::add_ssh_key))
        .route("/api/tokens", post(api::create_token))
        // --- collaboration: issues ---
        .route(
            "/api/orgs/{org}/repos/{repo}/issues",
            post(collab_api::create_issue).get(collab_api::list_issues),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/issues/{number}",
            get(collab_api::get_issue),
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
            get(collab_api::get_pr),
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
            post(collab_api::comment_pr),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/pulls/{number}/close",
            post(collab_api::close_pr),
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
            post(collab_api::set_protection).get(collab_api::get_protection),
        )
        // --- webhooks ---
        .route(
            "/api/orgs/{org}/repos/{repo}/hooks",
            post(collab_api::create_hook).get(collab_api::list_hooks),
        )
        // --- releases (JSON API) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/releases",
            post(releases_api::create_release).get(releases_api::list_releases),
        )
        .route(
            "/api/orgs/{org}/repos/{repo}/releases/{tag}/archive",
            get(releases_api::download_archive),
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
        .route("/api/search", get(governance_api::search))
        .route(
            "/api/users/{username}/notifications",
            get(governance_api::list_notifications),
        )
        // --- collaborators + teams (fine-grained RBAC) ---
        .route(
            "/api/orgs/{org}/repos/{repo}/collaborators",
            post(governance_api::add_collaborator),
        )
        .route("/api/orgs/{org}/teams", post(governance_api::create_team))
        .route(
            "/api/orgs/{org}/teams/{team}/members",
            post(governance_api::add_team_member),
        )
        .route(
            "/api/orgs/{org}/teams/{team}/repos",
            post(governance_api::grant_team_repo),
        )
        // Git smart + dumb HTTP: one catch-all per method.
        .route(
            "/{owner}/{repo}/{*path}",
            get(git_http::git_get).post(git_http::git_post),
        )
        // Server-rendered web UI (landing, repo browse, issues, PRs, releases).
        .merge(web::routes())
        .layer(DefaultBodyLimit::max(MAX_GIT_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness probe.
async fn healthz() -> &'static str {
    "ok"
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,rithub_server=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve on Ctrl-C so the server shuts down cleanly.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

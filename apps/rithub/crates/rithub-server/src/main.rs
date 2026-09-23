//! # rithub-server
//!
//! The axum HTTP service that makes rithub a real, self-hostable git forge:
//!
//! - a **forge JSON API** (`/api/...`) for organizations and repositories,
//!   backed by Postgres via `rithub-core`;
//! - a **dumb-HTTP git server** (`/{owner}/{repo}.git/...`) that hosts the
//!   `rit`-backed bare repositories, so a stock `git clone`/`git fetch` works;
//! - a **durable job worker** (tokio task) consuming `outbox_jobs` with
//!   retry + backoff for webhooks, notifications, audit, and email.
//!
//! Configuration is via environment (fail-closed on missing required values):
//!
//! | Var | Meaning | Default |
//! |-----|---------|---------|
//! | `DATABASE_URL` | Postgres connection string | *required* |
//! | `RITHUB_DATA_ROOT` | directory that holds bare repos | `./rithub-data` |
//! | `RITHUB_BIND` | listen address | `127.0.0.1:8080` |
//! | `SMTP_HOST` | outbound SMTP hostname | *(email disabled if absent)* |
//! | `SMTP_PORT` | SMTP port | `587` |
//! | `SMTP_USER` | SMTP username | *(required when SMTP_HOST is set)* |
//! | `SMTP_PASS` | SMTP password | *(required when SMTP_HOST is set)* |
//! | `SMTP_FROM` | From address | `rithub@<SMTP_HOST>` |

mod api;
mod audit;
mod auth;
mod ci;
mod collab_api;
mod error;
mod git_http;
mod governance_api;
mod graphql;
mod jobs;
mod jobs_api;
mod milestones_api;
mod releases_api;
mod search_api;
mod search_index;
mod ssh;
mod state;
mod web;
mod webhooks;

use std::path::PathBuf;

use anyhow::Context;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::Router;
use rithub_core::PgStore;
use rithub_git::RepoHost;
use sqlx::postgres::PgPoolOptions;
use tower_http::trace::TraceLayer;

use crate::state::{AppState, SmtpConfig};

/// Max git request body (a pushed pack).
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

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .context("connecting to Postgres")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    // Optional SMTP: configured when SMTP_HOST is present. Misconfigured SMTP
    // (bad creds, unreachable host) is non-fatal at startup — email jobs will
    // fail and retry until the config is corrected.
    let smtp_config = SmtpConfig::from_env();
    let mailer = smtp_config
        .as_ref()
        .and_then(|c| {
            c.build_transport()
                .inspect_err(|e| {
                    tracing::warn!(error = %e, "SMTP transport build failed — email disabled");
                })
                .ok()
        });
    if smtp_config.is_some() && mailer.is_some() {
        tracing::info!(host = %smtp_config.as_ref().unwrap().host, "SMTP transport ready");
    } else if smtp_config.is_some() {
        tracing::warn!("SMTP_HOST is set but transport build failed — check SMTP_* vars");
    } else {
        tracing::info!("SMTP not configured (SMTP_HOST unset) — email notifications disabled");
    }

    let state = AppState::new(
        PgStore::new(pool),
        RepoHost::new(&data_root),
        mailer,
        data_root.clone(),
    );

    // Full-text search: open the on-disk Tantivy index (default backend) and
    // attach it. Under `RITHUB_SEARCH_BACKEND=postgres` the native index is
    // skipped and the tsvector `search_documents` table answers queries.
    let state = if search_index::SearchBackend::from_env()
        == search_index::SearchBackend::Tantivy
    {
        let index_dir = data_root.join("search-index");
        match forge_search::SearchIndex::open(&index_dir) {
            Ok(idx) => {
                tracing::info!(dir = %index_dir.display(), "search index (tantivy) ready");
                state.with_search(idx)
            }
            Err(error) => {
                tracing::error!(%error, "opening search index failed — search degraded");
                state
            }
        }
    } else {
        tracing::info!("search backend = postgres (tsvector); native index disabled");
        state
    };

    // One-shot maintenance subcommand: `rithub-server reindex` rebuilds the
    // search index from Postgres + git, then exits.
    if std::env::args().nth(1).as_deref() == Some("reindex") {
        let n = search_index::reindex_all(&state).await?;
        tracing::info!(documents = n, "reindex complete");
        return Ok(());
    }

    // SSH transport: enabled when RITHUB_SSH_BIND is set.
    // The host key is persisted at $RITHUB_DATA_ROOT/ssh_host_key (ed25519,
    // generated on first boot) so clients can pin the server's fingerprint.
    if let Ok(ssh_bind) = std::env::var("RITHUB_SSH_BIND") {
        let rit_bin = std::env::var("RITHUB_RIT_BIN").unwrap_or_else(|_| "rit".to_string());
        let ssh_state = state.clone();
        let host_key_path = data_root.join("ssh_host_key");
        tokio::spawn(async move {
            if let Err(error) = ssh::serve(ssh_state, ssh_bind, rit_bin, &host_key_path).await {
                tracing::error!(%error, "SSH transport stopped");
            }
        });
    }

    // Durable job worker.
    {
        let worker_state = state.clone();
        tokio::spawn(async move {
            jobs::run_worker(worker_state).await;
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

fn build_router(state: AppState) -> Router {
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
            get(api::get_repo).patch(api::update_repo),
        )
        .route("/api/orgs/{org}/repos/{repo}/seed", post(api::seed_repo))
        .route("/api/orgs/{org}/repos/{repo}/forks", post(api::create_fork))
        .route("/api/orgs/{org}/members", post(api::add_member))
        .route("/api/users", post(api::create_user))
        .route("/api/users/{username}/keys", post(api::add_ssh_key))
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
            get(git_http::git_get).post(git_http::git_post),
        )
        // Server-rendered web UI (landing, repo browse, issues, PRs, releases).
        .merge(web::routes())
        .layer(DefaultBodyLimit::max(MAX_GIT_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

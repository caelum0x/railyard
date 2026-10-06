//! # switchyard-server
//!
//! The Switchyard HTTP API (`/v1/*`), MCP endpoint (`/mcp`), OAuth 2.1
//! authorization server (`/oauth/*`, `/.well-known/*`), `/admin` console,
//! `llms.txt` and `openapi.json`, all on one axum router.

pub mod config;
pub mod http_util;
pub mod pipeline;
pub mod principal;
pub mod routes;
pub mod routing;
pub mod state;
pub mod vaultops;

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, delete, get, post};
use axum::Router;
use switchyard_catalog::Catalog;

pub use config::Settings;
pub use state::{AppState, Deps};

use routes::{account, admin, mcp, meta, oauth, tools};

pub fn router(st: AppState) -> Router {
    Router::new()
        .route("/healthz", get(meta::healthz))
        .route("/readyz", get(meta::readyz))
        .route("/llms.txt", get(meta::llms_txt))
        .route("/openapi.json", get(meta::openapi))
        // Tools
        .route("/v1/tools/search", get(tools::search))
        .route("/v1/tools/batch", post(tools::batch))
        .route("/v1/tools/:id", get(tools::describe))
        .route("/v1/tools/:id/call", post(tools::call))
        .route("/v1/passthrough/:tool_id/*rest", any(tools::passthrough))
        // Account
        .route("/v1/keys", get(account::list_keys).post(account::create_key))
        .route("/v1/keys/:id", axum::routing::patch(account::patch_key))
        .route("/v1/keys/:id/revoke", post(account::revoke_key))
        .route("/v1/credentials", get(account::list_credentials).post(account::create_credential))
        .route("/v1/credentials/:id", delete(account::delete_credential))
        .route("/v1/credentials/oauth/:provider/connect", post(routes::oauth_connect::connect))
        .route("/v1/credentials/oauth/:provider/callback", get(routes::oauth_connect::callback))
        .route("/v1/bundles", get(account::list_bundles).post(account::push_bundle))
        .route("/v1/bundles/:id/skill.md", get(account::bundle_skill))
        .route("/v1/project", get(account::get_project).patch(account::patch_project))
        .route("/v1/usage", get(account::usage))
        .route("/v1/usage/calls", get(account::list_calls))
        .route("/v1/usage/calls/:id", get(account::get_call))
        .route("/v1/balance", get(account::balance))
        .route("/v1/balance/topup", post(account::topup))
        // MCP
        .route("/mcp", post(mcp::post).get(mcp::not_streaming).delete(mcp::not_streaming))
        // OAuth 2.1
        .route("/.well-known/oauth-authorization-server", get(oauth::as_metadata))
        .route("/.well-known/oauth-protected-resource", get(oauth::pr_metadata_api))
        .route("/.well-known/oauth-protected-resource/mcp", get(oauth::pr_metadata_mcp))
        .route("/oauth/jwks", get(oauth::jwks))
        .route("/oauth/register", post(oauth::register))
        .route("/oauth/authorize", get(oauth::authorize_page).post(oauth::authorize_submit))
        .route("/oauth/token", post(oauth::token))
        .route("/oauth/device/code", post(oauth::device_code))
        .route("/oauth/session", post(oauth::set_session))
        .route("/oauth/consent", get(oauth::tool_consent_page).post(oauth::tool_consent_submit))
        .route("/device", get(oauth::device_page).post(oauth::device_submit))
        // Operator
        .route("/admin", get(admin::console))
        .route("/admin/api/overview", get(admin::overview))
        .route("/admin/api/orgs", post(admin::create_org))
        .route("/admin/api/projects", post(admin::create_project))
        .route("/admin/api/members", post(admin::add_member))
        .route("/admin/api/keys", post(admin::create_key))
        .route("/admin/api/byo-tools", post(admin::create_byo))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(st)
}

/// The static egress allowlist: provider hosts plus the hosts of configured
/// own-product and self-hosted base URLs.
pub fn egress_allowlist(catalog: &Catalog, adapter_config: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut hosts = catalog.egress_hosts();
    for p in catalog.providers() {
        if let Some(key) = &p.egress_host_from_config {
            if let Some(h) = adapter_config.get(key).and_then(|u| url::Url::parse(u).ok()).and_then(|u| u.host_str().map(str::to_string)) {
                hosts.insert(h);
            }
        }
    }
    hosts
}

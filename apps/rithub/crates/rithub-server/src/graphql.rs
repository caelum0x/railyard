//! GraphQL transport: mounts the `rithub-graphql` schema at `/graphql`, wires
//! the on-disk git host into it, authenticates each request exactly like the
//! REST/git plane, and (in dev) serves a GraphiQL playground.
//!
//! ## Auth parity
//! Every GraphQL request is authenticated with [`crate::auth::authenticate`] —
//! the same HTTP Basic → PAT / deploy-key path REST and git use. The resulting
//! [`Caller`] is mapped onto a [`rithub_graphql::Viewer`] and injected into the
//! request, so resolver-level authorization is identical to REST.

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use rithub_git::RepoHost;
use rithub_graphql::{GitHost, GitHostHandle, RithubSchema, Viewer};
use std::sync::Arc;

use crate::auth::{authenticate, Caller};
use crate::state::AppState;

/// Adapts the concrete [`RepoHost`] to the `rithub-graphql` [`GitHost`] port, so
/// the GraphQL crate stays free of git internals.
struct RepoHostAdapter {
    host: RepoHost,
}

impl GitHost for RepoHostAdapter {
    fn init_bare(&self, org: &str, repo: &str, default_branch: &str) -> Result<String, String> {
        self.host
            .init_bare(org, repo, default_branch)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|e| e.to_string())
    }

    fn resolve_branch_sha(&self, org: &str, repo: &str, branch: &str) -> Option<String> {
        self.host
            .resolve_branch(org, repo, branch)
            .ok()
            .map(|oid| oid.to_hex())
    }
}

/// Build the forge GraphQL schema, wiring the store and the on-disk git host.
pub fn build_schema(store: rithub_core::PgStore, host: RepoHost) -> RithubSchema {
    let git_host: GitHostHandle = Arc::new(RepoHostAdapter { host });
    rithub_graphql::build_schema(store, git_host)
}

/// Map an authenticated REST [`Caller`] (or anonymous) onto a GraphQL [`Viewer`].
fn to_viewer(caller: Option<Caller>) -> Viewer {
    match caller {
        Some(Caller::User { user, scopes }) => Viewer::User { user, scopes },
        Some(Caller::DeployKey { repo_id, read_only }) => {
            Viewer::DeployKey { repo_id, read_only }
        }
        None => Viewer::Anonymous,
    }
}

/// `POST /graphql` — execute a GraphQL query/mutation. Authenticates the request
/// and injects the resolved viewer, then runs it against the schema.
pub async fn graphql_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> Response {
    // Same authentication as REST/git; invalid credentials → 401 challenge.
    let caller = match authenticate(&state, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let viewer = to_viewer(caller);
    let request = req.into_inner().data(viewer);
    let response: GraphQLResponse = state.graphql.execute(request).await.into();
    response.into_response()
}

/// `GET /graphql` — a GraphiQL playground, enabled only when `RITHUB_GRAPHIQL`
/// is set (development). Otherwise a hint that the endpoint is POST-only.
pub async fn graphiql() -> Response {
    if std::env::var("RITHUB_GRAPHIQL").is_err() {
        return (
            StatusCode::NOT_FOUND,
            "GraphiQL disabled; set RITHUB_GRAPHIQL=1 to enable. POST GraphQL to /graphql.",
        )
            .into_response();
    }
    Html(GraphiQLSource::build().endpoint("/graphql").finish()).into_response()
}

/// `GET /graphql/schema` — export the schema SDL for tooling and client codegen.
/// Available to authenticated callers (or everyone when `RITHUB_GRAPHIQL` is set).
pub async fn graphql_schema(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if std::env::var("RITHUB_GRAPHIQL").is_err() {
        match crate::authz::api_caller(&state, &headers).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return (StatusCode::UNAUTHORIZED, "authentication required").into_response()
            }
            Err(e) => return axum::response::IntoResponse::into_response(e),
        }
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        state.graphql.sdl(),
    )
        .into_response()
}

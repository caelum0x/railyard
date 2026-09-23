//! GraphQL transport: mounts the `rithub-graphql` schema at `/graphql`, wires
//! the on-disk git host into it, authenticates each request exactly like the
//! REST/git plane, and (in dev) serves a GraphiQL playground.
//!
//! ## Auth parity
//! Every GraphQL request is authenticated with [`crate::auth::authenticate`] —
//! the same HTTP Basic → PAT / deploy-key path REST and git use. The resulting
//! [`Caller`] is mapped onto a [`rithub_graphql::Viewer`] and injected into the
//! request, so resolver-level authorization is identical to REST.
//!
//! ## Side-effect parity
//! A [`ForgeServices`] implementation over the server's service layer
//! (`api::create_repo_in`, `api::update_repo_settings`, `collab_svc`) is also
//! injected per request, so GraphQL writes enforce the private-repo quota,
//! move repos on rename, and fire webhooks/notifications/audit/CI/indexing.

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use async_trait::async_trait;
use rithub_core::{
    Issue, IssueComment, NewRepository, Organization, PrComment, PullRequest, Repository,
    UpdateRepository, User,
};
use rithub_git::RepoHost;
use rithub_graphql::{
    ForgeServices, ForgeServicesHandle, GitHost, GitHostHandle, IssueEdit, PullRequestEdit,
    PullRequestInput, RithubSchema, ServiceError, Viewer,
};
use std::sync::Arc;

use crate::auth::{authenticate, Caller};
use crate::collab_svc::{self, IssuePatch, PrInput, PrPatch, RepoRef};
use crate::error::AppError;
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

/// Map a service-layer [`AppError`] onto a client-safe GraphQL service error.
fn to_service_error(err: AppError) -> ServiceError {
    let code = match err.status() {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => "BAD_REQUEST",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "CONFLICT",
        StatusCode::FORBIDDEN => "FORBIDDEN",
        StatusCode::UNAUTHORIZED => "UNAUTHENTICATED",
        StatusCode::PAYMENT_REQUIRED => "PAYMENT_REQUIRED",
        StatusCode::SERVICE_UNAVAILABLE => "UNAVAILABLE",
        _ => "INTERNAL",
    };
    ServiceError {
        code,
        message: err.client_message(),
    }
}

/// The GraphQL service port over the server's real service layer, so GraphQL
/// writes fire exactly the same side effects as REST and the web UI.
struct ServerServices {
    state: AppState,
}

#[async_trait]
impl ForgeServices for ServerServices {
    async fn create_repository(
        &self,
        actor: &User,
        org: &Organization,
        input: &NewRepository,
    ) -> Result<Repository, ServiceError> {
        crate::api::create_repo_in(&self.state, org, actor, input)
            .await
            .map_err(to_service_error)
    }

    async fn update_repository(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        input: &UpdateRepository,
    ) -> Result<Repository, ServiceError> {
        crate::api::update_repo_settings(&self.state, org, repo, actor, input)
            .await
            .map_err(to_service_error)
    }

    async fn create_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        title: &str,
        body: Option<String>,
    ) -> Result<Issue, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        collab_svc::create_issue(&self.state, &r, actor, title, body)
            .await
            .map_err(to_service_error)
    }

    async fn update_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        edit: IssueEdit,
    ) -> Result<Issue, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        let patch = IssuePatch {
            title: edit.title,
            body: edit.body,
            state: edit.state,
            milestone: edit.milestone,
            assignees: None,
            labels: None,
        };
        collab_svc::update_issue(&self.state, &r, number, actor, patch)
            .await
            .map(|detail| detail.issue)
            .map_err(to_service_error)
    }

    async fn comment_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        body: &str,
    ) -> Result<IssueComment, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        collab_svc::comment_issue(&self.state, &r, number, actor, body)
            .await
            .map_err(to_service_error)
    }

    async fn create_pull_request(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        input: PullRequestInput,
    ) -> Result<PullRequest, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        let input = PrInput {
            title: input.title,
            body: input.body,
            head: input.head,
            base: input.base,
            head_repo: input.head_repo,
        };
        collab_svc::create_pr(&self.state, &r, actor, input)
            .await
            .map_err(to_service_error)
    }

    async fn update_pull_request(
        &self,
        _actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        edit: PullRequestEdit,
    ) -> Result<PullRequest, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        let patch = PrPatch {
            title: edit.title,
            body: edit.body,
            base: edit.base,
            milestone: edit.milestone,
        };
        collab_svc::update_pr(&self.state, &r, number, patch)
            .await
            .map_err(to_service_error)
    }

    async fn set_pull_request_state(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        open: bool,
    ) -> Result<PullRequest, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        collab_svc::set_pr_state(&self.state, &r, number, actor, open)
            .await
            .map_err(to_service_error)
    }

    async fn comment_pull_request(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        body: &str,
    ) -> Result<PrComment, ServiceError> {
        let r = RepoRef { org, repo: &repo.name, meta: repo };
        collab_svc::comment_pr(&self.state, &r, number, actor, body)
            .await
            .map_err(to_service_error)
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
    let services: ForgeServicesHandle = Arc::new(ServerServices {
        state: state.clone(),
    });
    let request = req.into_inner().data(viewer).data(services);
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

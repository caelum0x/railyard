//! The forge **service port**: every GraphQL mutation that has side effects
//! beyond a single row write goes through this trait instead of the store.
//!
//! `rithub-server` implements it over its service layer (`api::create_repo_in`,
//! `api::update_repo_settings`, `collab_svc`, `pulls_svc`), so a repository,
//! issue, pull request, or comment created over GraphQL behaves exactly like one
//! created over REST or the web UI: name validation, the private-repo plan quota
//! and metering, on-disk rename, webhooks, notifications, email, audit, CI
//! dispatch, and search indexing. This crate stays free of those internals.
//!
//! The transport injects a [`ForgeServicesHandle`] per request (next to the
//! [`crate::Viewer`]); resolvers fetch it with [`crate::context::services`].

use std::sync::Arc;

use async_trait::async_trait;
use rithub_core::{
    Issue, IssueComment, IssueState, NewRepository, Organization, PrComment, PullRequest,
    Repository, UpdateRepository, User,
};

/// A service-layer failure, already made client-safe by the implementor.
#[derive(Debug, Clone)]
pub struct ServiceError {
    /// Stable machine-readable code (`BAD_REQUEST`, `NOT_FOUND`, `CONFLICT`,
    /// `FORBIDDEN`, `UNAUTHENTICATED`, `PAYMENT_REQUIRED`, `UNAVAILABLE`,
    /// `INTERNAL`).
    pub code: &'static str,
    /// Client-safe message (internal details are never included).
    pub message: String,
}

/// A partial issue edit applied through the service layer.
#[derive(Debug, Default, Clone)]
pub struct IssueEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `Some(0)` clears the milestone; `Some(n)` sets milestone number `n`.
    pub milestone: Option<i32>,
    pub state: Option<IssueState>,
}

/// Input for opening a pull request through the service layer.
#[derive(Debug, Clone)]
pub struct PullRequestInput {
    pub title: String,
    pub body: Option<String>,
    pub head: String,
    pub base: String,
    /// Source `org/repo` for a cross-repository (fork) pull request.
    pub head_repo: Option<String>,
}

/// A partial pull-request metadata edit.
#[derive(Debug, Default, Clone)]
pub struct PullRequestEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    /// `Some(0)` clears the milestone; `Some(n)` sets milestone number `n`.
    pub milestone: Option<i32>,
}

/// The side-effecting forge operations GraphQL mutations delegate to. Callers
/// authorize first; implementations perform the write and fan out effects.
#[async_trait]
pub trait ForgeServices: Send + Sync {
    /// Create a repository in `org` (name validation, private quota, on-disk
    /// init, audit, metering, search indexing).
    async fn create_repository(
        &self,
        actor: &User,
        org: &Organization,
        input: &NewRepository,
    ) -> Result<Repository, ServiceError>;

    /// Apply repository settings (rename moves the repo on disk; default branch
    /// must exist and updates HEAD; going private checks the quota).
    async fn update_repository(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        input: &UpdateRepository,
    ) -> Result<Repository, ServiceError>;

    async fn create_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        title: &str,
        body: Option<String>,
    ) -> Result<Issue, ServiceError>;

    async fn update_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        edit: IssueEdit,
    ) -> Result<Issue, ServiceError>;

    async fn comment_issue(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        body: &str,
    ) -> Result<IssueComment, ServiceError>;

    async fn create_pull_request(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        input: PullRequestInput,
    ) -> Result<PullRequest, ServiceError>;

    async fn update_pull_request(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        edit: PullRequestEdit,
    ) -> Result<PullRequest, ServiceError>;

    /// Close (`open = false`) or reopen (`open = true`) a pull request.
    async fn set_pull_request_state(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        open: bool,
    ) -> Result<PullRequest, ServiceError>;

    async fn comment_pull_request(
        &self,
        actor: &User,
        org: &str,
        repo: &Repository,
        number: i32,
        body: &str,
    ) -> Result<PrComment, ServiceError>;
}

/// A shared [`ForgeServices`] handle, injected per request.
pub type ForgeServicesHandle = Arc<dyn ForgeServices>;

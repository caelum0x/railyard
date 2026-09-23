//! # rithub-graphql
//!
//! The forge **GraphQL API** (the plan's `forge-graphql`): a `Query` +
//! `Mutation` schema over the same `rithub-core` domain store the REST/git
//! plane uses, with identical authorization.
//!
//! This crate owns **no** git internals and does not depend on `rithub-git`/
//! `rit`. On-disk repository creation is delegated through the [`GitHost`] trait,
//! which the transport crate (`rithub-server`) implements over its `RepoHost`.
//! That keeps the GraphQL layer independently buildable and testable.
//!
//! ## Wiring
//!
//! ```ignore
//! use std::sync::Arc;
//! let schema = rithub_graphql::build_schema(store, Arc::new(my_git_host));
//! // Per request: authenticate exactly like REST, then inject the Viewer:
//! let response = schema.execute(request.data(viewer)).await;
//! ```
//!
//! Side-effecting mutations (repo create/update, issues, PRs, comments) are
//! delegated through the [`ForgeServices`] port, injected per request with
//! `request.data(services_handle)`, so GraphQL writes fire the same webhooks,
//! notifications, audit, billing, CI, and indexing as REST.
//!
//! The SDL can be exported for tooling/codegen with [`RithubSchema::sdl`].

mod authz;
mod context;
mod error;
mod mutation;
mod query;
mod services;
mod types;

pub use context::{git_host, services, store, viewer, GitHost, GitHostHandle, Viewer};
pub use services::{
    ForgeServices, ForgeServicesHandle, IssueEdit, PullRequestEdit, PullRequestInput, ServiceError,
};
pub use mutation::{MilestoneStateInput, Mutation, PrStateInput};
pub use query::Query;
pub use types::{
    IssueCommentObject, IssueObject, IssueStateEnum, MilestoneObject, OrganizationObject,
    PrCommentObject, PrStateEnum, PullRequestObject, RepositoryObject, RoleEnum, UserObject,
    VisibilityEnum,
};

use async_graphql::{EmptySubscription, Schema};
use rithub_core::PgStore;

/// The concrete forge schema type.
pub type RithubSchema = Schema<Query, Mutation, EmptySubscription>;

/// Build the GraphQL schema, wiring the shared metadata store and the on-disk
/// git-host delegate as schema-level data. The per-request [`Viewer`] is added
/// by the transport via `request.data(viewer)`.
pub fn build_schema(store: PgStore, git_host: GitHostHandle) -> RithubSchema {
    Schema::build(Query, Mutation, EmptySubscription)
        .data(store)
        .data(git_host)
        .finish()
}

/// Export the schema's SDL (Schema Definition Language) as a string, for schema
/// export, client codegen, and contract checks.
pub fn schema_sdl(store: PgStore, git_host: GitHostHandle) -> String {
    build_schema(store, git_host).sdl()
}

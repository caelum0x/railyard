//! The GraphQL `Query` root: read access to orgs, repos, users, issues, PRs,
//! and milestones — with the same authorization as the REST/git plane, plus
//! filters and offset pagination.
//!
//! Pagination is offset-based (`limit`/`offset` arguments). The domain store
//! returns full result sets for these small per-repo/per-org collections, so
//! the slice is applied in-memory here; the store stays unchanged.

use async_graphql::{Context, Object};

use rithub_core::{
    IssueState, IssueStore, MilestoneStore, PrState, PullRequestStore, Store,
};

use crate::authz::{authorize_read, can_read};
use crate::context::{store, viewer};
use crate::error::core_error;
use crate::types::{
    IssueObject, IssueStateEnum, MilestoneObject, OrganizationObject, PrStateEnum,
    PullRequestObject, RepositoryObject, UserObject,
};

/// Default page size when `limit` is omitted.
const DEFAULT_LIMIT: usize = 50;
/// Hard cap so a single query can never ask for an unbounded page.
const MAX_LIMIT: usize = 100;

/// Clamp/normalize `(limit, offset)` arguments into `(take, skip)`.
fn page(limit: Option<i32>, offset: Option<i32>) -> (usize, usize) {
    let take = match limit {
        Some(n) if n > 0 => (n as usize).min(MAX_LIMIT),
        _ => DEFAULT_LIMIT,
    };
    let skip = match offset {
        Some(n) if n > 0 => n as usize,
        _ => 0,
    };
    (take, skip)
}

/// Apply an offset window to a vector, mapping each element with `f`.
fn paginate<T, U>(items: Vec<T>, limit: Option<i32>, offset: Option<i32>, f: impl Fn(T) -> U) -> Vec<U> {
    let (take, skip) = page(limit, offset);
    items.into_iter().skip(skip).take(take).map(f).collect()
}

/// The read root.
pub struct Query;

#[Object]
impl Query {
    /// API sanity probe.
    async fn api_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    /// List organizations, newest first.
    async fn organizations(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> async_graphql::Result<Vec<OrganizationObject>> {
        let orgs = store(ctx)?.list_orgs().await.map_err(core_error)?;
        Ok(paginate(orgs, limit, offset, OrganizationObject::from))
    }

    /// Fetch one organization by name.
    async fn organization(
        &self,
        ctx: &Context<'_>,
        name: String,
    ) -> async_graphql::Result<OrganizationObject> {
        let org = store(ctx)?.get_org(&name).await.map_err(core_error)?;
        Ok(org.into())
    }

    /// Fetch one user by username.
    async fn user(&self, ctx: &Context<'_>, username: String) -> async_graphql::Result<UserObject> {
        let user = store(ctx)?.get_user(&username).await.map_err(core_error)?;
        Ok(user.into())
    }

    /// List repositories in an organization. Private repositories the viewer
    /// cannot read are filtered out before pagination.
    async fn repositories(
        &self,
        ctx: &Context<'_>,
        org: String,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> async_graphql::Result<Vec<RepositoryObject>> {
        let st = store(ctx)?;
        let view = viewer(ctx);
        let org = st.get_org(&org).await.map_err(core_error)?;
        let repos = st.list_repos(&org).await.map_err(core_error)?;

        // Authorization filter: never leak a private repo into a list result.
        let mut visible = Vec::with_capacity(repos.len());
        for repo in repos {
            if can_read(st, &repo, view).await {
                visible.push(repo);
            }
        }
        Ok(paginate(visible, limit, offset, RepositoryObject::from))
    }

    /// Fetch a single repository (authorized: private repos require ≥Read).
    async fn repository(
        &self,
        ctx: &Context<'_>,
        org: String,
        name: String,
    ) -> async_graphql::Result<RepositoryObject> {
        let st = store(ctx)?;
        let repo = st.get_repo(&org, &name).await.map_err(core_error)?;
        authorize_read(st, &repo, viewer(ctx)).await?;
        Ok(repo.into())
    }

    /// List a repository's issues, optionally filtered by state.
    async fn issues(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        state: Option<IssueStateEnum>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> async_graphql::Result<Vec<IssueObject>> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let filter: Option<IssueState> = state.map(Into::into);
        let issues = st
            .list_issues(repository.id, filter)
            .await
            .map_err(core_error)?;
        Ok(paginate(issues, limit, offset, IssueObject::from))
    }

    /// Fetch one issue by number.
    async fn issue(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
    ) -> async_graphql::Result<IssueObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let issue = st
            .get_issue(repository.id, number)
            .await
            .map_err(core_error)?;
        Ok(issue.into())
    }

    /// List a repository's pull requests, optionally filtered by state.
    async fn pull_requests(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        state: Option<PrStateEnum>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> async_graphql::Result<Vec<PullRequestObject>> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let filter: Option<PrState> = state.map(Into::into);
        let prs = st
            .list_prs(repository.id, filter)
            .await
            .map_err(core_error)?;
        Ok(paginate(prs, limit, offset, PullRequestObject::from))
    }

    /// Fetch one pull request by number.
    async fn pull_request(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
    ) -> async_graphql::Result<PullRequestObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let pr = st.get_pr(repository.id, number).await.map_err(core_error)?;
        Ok(pr.into())
    }

    /// List a repository's milestones (with live progress counts).
    async fn milestones(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> async_graphql::Result<Vec<MilestoneObject>> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let milestones = st
            .list_milestones(repository.id)
            .await
            .map_err(core_error)?;
        Ok(paginate(milestones, limit, offset, MilestoneObject::from))
    }

    /// Fetch one milestone by number.
    async fn milestone(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
    ) -> async_graphql::Result<MilestoneObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let milestone = st
            .get_milestone(repository.id, number)
            .await
            .map_err(core_error)?;
        Ok(milestone.into())
    }
}

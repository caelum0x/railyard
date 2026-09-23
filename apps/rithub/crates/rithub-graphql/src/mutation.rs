//! The GraphQL `Mutation` root: the same creates/updates the REST API exposes —
//! create/update repository, issue, pull request, comment, and milestone.
//!
//! Authorization reuses the shared rules in [`crate::authz`]. Every mutation
//! with side effects (repositories, issues, pull requests, comments) is then
//! delegated to the forge service layer through [`crate::ForgeServices`], so it
//! enforces the same validation, plan quota, on-disk moves, webhooks,
//! notifications, audit, CI dispatch, and search indexing as REST and the web.

use async_graphql::{Context, Object};
use time::OffsetDateTime;

use rithub_core::{
    MilestoneStore, NewMilestone, NewRepository, Store, UpdateMilestone, UpdateRepository, User,
    Visibility,
};

use crate::authz::{authorize_admin, authorize_org_manage, authorize_read, authorize_write};
use crate::context::{services, store, viewer, Viewer};
use crate::error::{core_error, service_error, unauthenticated};
use crate::services::{IssueEdit, PullRequestEdit, PullRequestInput};
use crate::types::{
    IssueCommentObject, IssueObject, IssueStateEnum, MilestoneObject, PrCommentObject,
    PullRequestObject, RepositoryObject, VisibilityEnum,
};

/// Require an authenticated user (mutations always attribute authorship to the
/// calling user; deploy keys/anonymous cannot author).
fn author(viewer: &Viewer) -> async_graphql::Result<&User> {
    viewer.user().ok_or_else(unauthenticated)
}

fn to_visibility(v: VisibilityEnum) -> Visibility {
    match v {
        VisibilityEnum::Public => Visibility::Public,
        VisibilityEnum::Private => Visibility::Private,
    }
}

/// The write root.
pub struct Mutation;

#[Object]
impl Mutation {
    /// Create a repository. Requires org admin/owner (same authority REST
    /// expects); enforces name rules and the org's private-repo plan quota.
    async fn create_repository(
        &self,
        ctx: &Context<'_>,
        org: String,
        name: String,
        description: Option<String>,
        default_branch: Option<String>,
        visibility: Option<VisibilityEnum>,
    ) -> async_graphql::Result<RepositoryObject> {
        let st = store(ctx)?;
        let organization = st.get_org(&org).await.map_err(core_error)?;
        authorize_org_manage(st, organization.id, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let input = NewRepository {
            name,
            description,
            default_branch,
            visibility: visibility.map(to_visibility),
        };
        let repo = services(ctx)?
            .create_repository(actor, &organization, &input)
            .await
            .map_err(service_error)?;
        Ok(repo.into())
    }

    /// Update repository settings (name, description, default branch,
    /// visibility). Requires Admin on the repository, like REST. A rename moves
    /// the repository on disk; making it private checks the plan quota.
    async fn update_repository(
        &self,
        ctx: &Context<'_>,
        org: String,
        name: String,
        new_name: Option<String>,
        description: Option<String>,
        default_branch: Option<String>,
        visibility: Option<VisibilityEnum>,
    ) -> async_graphql::Result<RepositoryObject> {
        let st = store(ctx)?;
        let repo = st.get_repo(&org, &name).await.map_err(core_error)?;
        authorize_admin(st, &repo, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let patch = UpdateRepository {
            name: new_name,
            description,
            default_branch,
            visibility: visibility.map(to_visibility),
        };
        let updated = services(ctx)?
            .update_repository(actor, &org, &repo, &patch)
            .await
            .map_err(service_error)?;
        Ok(updated.into())
    }

    /// Open an issue. Requires an authenticated user with ≥Read on the repo.
    async fn create_issue(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        title: String,
        body: Option<String>,
    ) -> async_graphql::Result<IssueObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let issue = services(ctx)?
            .create_issue(actor, &org, &repository, &title, body)
            .await
            .map_err(service_error)?;
        Ok(issue.into())
    }

    /// Update an issue's title/body/milestone and/or open-closed state.
    /// Requires ≥Write on the repository.
    async fn update_issue(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        title: Option<String>,
        body: Option<String>,
        milestone_number: Option<i32>,
        clear_milestone: Option<bool>,
        state: Option<IssueStateEnum>,
    ) -> async_graphql::Result<IssueObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_write(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let milestone = match (milestone_number, clear_milestone) {
            (Some(num), _) => Some(num),
            (None, Some(true)) => Some(0),
            _ => None,
        };
        let issue = services(ctx)?
            .update_issue(
                actor,
                &org,
                &repository,
                number,
                IssueEdit {
                    title,
                    body,
                    milestone,
                    state: state.map(Into::into),
                },
            )
            .await
            .map_err(service_error)?;
        Ok(issue.into())
    }

    /// Open a pull request. Both branches must exist; `headRepo` (`org/repo`)
    /// opens a cross-repository PR from a fork the caller can read. Requires an
    /// authenticated user with ≥Read on the target repo.
    async fn create_pull_request(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        title: String,
        body: Option<String>,
        head: String,
        base: String,
        head_repo: Option<String>,
    ) -> async_graphql::Result<PullRequestObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let pr = services(ctx)?
            .create_pull_request(
                actor,
                &org,
                &repository,
                PullRequestInput {
                    title,
                    body,
                    head,
                    base,
                    head_repo,
                },
            )
            .await
            .map_err(service_error)?;
        Ok(pr.into())
    }

    /// Update a pull request's title/body/base branch/milestone, and/or
    /// transition its open-closed state (`OPEN`/`CLOSED`). Merging goes through
    /// the merge endpoint, not here. Requires ≥Write on the repository.
    async fn update_pull_request(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        title: Option<String>,
        body: Option<String>,
        base: Option<String>,
        milestone_number: Option<i32>,
        clear_milestone: Option<bool>,
        state: Option<PrStateInput>,
    ) -> async_graphql::Result<PullRequestObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_write(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;
        let svc = services(ctx)?;

        let milestone = match (milestone_number, clear_milestone) {
            (Some(num), _) => Some(num),
            (None, Some(true)) => Some(0),
            _ => None,
        };
        let mut pr = None;
        if title.is_some() || body.is_some() || base.is_some() || milestone.is_some() {
            pr = Some(
                svc.update_pull_request(
                    actor,
                    &org,
                    &repository,
                    number,
                    PullRequestEdit {
                        title,
                        body,
                        base,
                        milestone,
                    },
                )
                .await
                .map_err(service_error)?,
            );
        }
        if let Some(target) = state {
            pr = Some(
                svc.set_pull_request_state(
                    actor,
                    &org,
                    &repository,
                    number,
                    target == PrStateInput::Open,
                )
                .await
                .map_err(service_error)?,
            );
        }
        let pr = match pr {
            Some(pr) => pr,
            None => {
                use rithub_core::PullRequestStore;
                st.get_pr(repository.id, number).await.map_err(core_error)?
            }
        };
        Ok(pr.into())
    }

    /// Add a comment to an issue. Requires an authenticated user with ≥Read.
    async fn add_issue_comment(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        body: String,
    ) -> async_graphql::Result<IssueCommentObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let comment = services(ctx)?
            .comment_issue(actor, &org, &repository, number, &body)
            .await
            .map_err(service_error)?;
        Ok(comment.into())
    }

    /// Add a comment to a pull request. Requires an authenticated user with ≥Read.
    async fn add_pull_request_comment(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        body: String,
    ) -> async_graphql::Result<PrCommentObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let actor = author(viewer(ctx))?;

        let comment = services(ctx)?
            .comment_pull_request(actor, &org, &repository, number, &body)
            .await
            .map_err(service_error)?;
        Ok(comment.into())
    }

    /// Create a milestone. Requires ≥Write on the repository.
    async fn create_milestone(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        title: String,
        description: Option<String>,
        due_on: Option<OffsetDateTime>,
    ) -> async_graphql::Result<MilestoneObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_write(st, &repository, viewer(ctx)).await?;
        if title.trim().is_empty() {
            return Err(crate::error::bad_request("milestone title is required"));
        }

        let milestone = st
            .create_milestone(
                repository.id,
                &NewMilestone {
                    title,
                    description,
                    due_on,
                },
            )
            .await
            .map_err(core_error)?;
        // Re-fetch with progress counts (0/0 for a fresh milestone).
        let with_progress = st
            .get_milestone(repository.id, milestone.number)
            .await
            .map_err(core_error)?;
        Ok(with_progress.into())
    }

    /// Update a milestone's title/description/due date and/or open-closed state
    /// (`state: "open" | "closed"`). Requires ≥Write on the repository.
    async fn update_milestone(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        title: Option<String>,
        description: Option<String>,
        due_on: Option<OffsetDateTime>,
        state: Option<MilestoneStateInput>,
    ) -> async_graphql::Result<MilestoneObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_write(st, &repository, viewer(ctx)).await?;

        let state_str = state.map(|s| match s {
            MilestoneStateInput::Open => "open".to_string(),
            MilestoneStateInput::Closed => "closed".to_string(),
        });

        st.update_milestone(
            repository.id,
            number,
            &UpdateMilestone {
                title,
                description,
                due_on,
                state: state_str,
            },
        )
        .await
        .map_err(core_error)?;

        let with_progress = st
            .get_milestone(repository.id, number)
            .await
            .map_err(core_error)?;
        Ok(with_progress.into())
    }
}

/// Target open-closed state for a pull-request update.
#[derive(async_graphql::Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PrStateInput {
    Open,
    Closed,
}

/// Target open-closed state for a milestone update.
#[derive(async_graphql::Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum MilestoneStateInput {
    Open,
    Closed,
}

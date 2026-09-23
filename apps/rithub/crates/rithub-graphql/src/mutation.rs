//! The GraphQL `Mutation` root: the same creates/updates the REST API exposes —
//! create/update repository, issue, pull request, comment, and milestone —
//! reusing the domain store and the shared authorization rules.

use async_graphql::{Context, Object};
use time::OffsetDateTime;

use rithub_core::{
    IssueState, IssueStore, MilestoneStore, NewIssue, NewMilestone, NewPullRequest, NewRepository,
    PullRequestStore, Store, UpdateIssue, UpdateMilestone, UpdatePr, UpdateRepository, Visibility,
};

use crate::authz::{authorize_org_manage, authorize_read, authorize_write};
use crate::context::{git_host, store, viewer, Viewer};
use crate::error::{bad_request, core_error, unauthenticated};
use crate::types::{
    IssueCommentObject, IssueObject, IssueStateEnum, MilestoneObject, PrCommentObject,
    PullRequestObject, RepositoryObject, VisibilityEnum,
};

/// Require an authenticated user and return their id (mutations always attribute
/// authorship to the calling user; deploy keys/anonymous cannot author).
fn author_id(viewer: &Viewer) -> async_graphql::Result<uuid::Uuid> {
    viewer.user().map(|u| u.id).ok_or_else(unauthenticated)
}

/// The write root.
pub struct Mutation;

#[Object]
impl Mutation {
    /// Create a repository: initialize the bare repo on disk, then record the
    /// metadata row. Requires org admin/owner (same authority REST expects).
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

        let branch = default_branch.clone().unwrap_or_else(|| "main".to_string());
        let disk_path = git_host(ctx)?
            .init_bare(&organization.name, &name, &branch)
            .map_err(bad_request)?;

        let input = NewRepository {
            name,
            description,
            default_branch,
            visibility: visibility.map(|v| match v {
                VisibilityEnum::Public => Visibility::Public,
                VisibilityEnum::Private => Visibility::Private,
            }),
        };
        let repo = st
            .create_repo(&organization, &input, &disk_path)
            .await
            .map_err(core_error)?;
        Ok(repo.into())
    }

    /// Update mutable repository metadata (name, description, default branch,
    /// visibility). Requires ≥Write on the repository.
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
        authorize_write(st, &repo, viewer(ctx)).await?;

        let patch = UpdateRepository {
            name: new_name,
            description,
            default_branch,
            visibility: visibility.map(|v| match v {
                VisibilityEnum::Public => Visibility::Public,
                VisibilityEnum::Private => Visibility::Private,
            }),
        };
        let updated = st.update_repo(repo.id, &patch).await.map_err(core_error)?;
        Ok(updated.into())
    }

    /// Open an issue. Requires an authenticated user with ≥Read on the repo
    /// (anonymous/deploy-key callers cannot author).
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
        let author = author_id(viewer(ctx))?;

        let issue = st
            .create_issue(repository.id, Some(author), &NewIssue { title, body })
            .await
            .map_err(core_error)?;
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

        // Resolve an optional milestone reference into the store's set/clear form.
        let (set_milestone, milestone_id) = match (milestone_number, clear_milestone) {
            (Some(num), _) => {
                let id = st
                    .get_milestone_id(repository.id, num)
                    .await
                    .map_err(core_error)?;
                (true, Some(id))
            }
            (None, Some(true)) => (true, None),
            _ => (false, None),
        };

        // Apply metadata changes when any were supplied.
        if title.is_some() || body.is_some() || set_milestone {
            st.update_issue(
                repository.id,
                number,
                &UpdateIssue {
                    title,
                    body,
                    set_milestone,
                    milestone_id,
                },
            )
            .await
            .map_err(core_error)?;
        }

        // Apply a state transition when requested.
        if let Some(state) = state {
            st.set_issue_state(repository.id, number, IssueState::from(state))
                .await
                .map_err(core_error)?;
        }

        let issue = st
            .get_issue(repository.id, number)
            .await
            .map_err(core_error)?;
        Ok(issue.into())
    }

    /// Open a pull request. Base/head shas are pinned best-effort from the live
    /// branch tips; mergeability and the diff are computed lazily by the git
    /// endpoints. Requires an authenticated user with ≥Read on the repo.
    async fn create_pull_request(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        title: String,
        body: Option<String>,
        head: String,
        base: String,
    ) -> async_graphql::Result<PullRequestObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_read(st, &repository, viewer(ctx)).await?;
        let author = author_id(viewer(ctx))?;

        let host = git_host(ctx)?;
        let base_sha = host.resolve_branch_sha(&org, &repo, &base);
        let head_sha = host.resolve_branch_sha(&org, &repo, &head);

        let pr = st
            .create_pr(
                repository.id,
                Some(author),
                &NewPullRequest {
                    title,
                    body,
                    head,
                    base,
                },
                base_sha.as_deref(),
                head_sha.as_deref(),
                None,
            )
            .await
            .map_err(core_error)?;
        Ok(pr.into())
    }

    /// Update a pull request's title/body/base branch, and/or transition its
    /// open-closed state (`OPEN`/`CLOSED`). Merging goes through the git merge
    /// endpoint, not here. Requires ≥Write on the repository.
    async fn update_pull_request(
        &self,
        ctx: &Context<'_>,
        org: String,
        repo: String,
        number: i32,
        title: Option<String>,
        body: Option<String>,
        base: Option<String>,
        state: Option<PrStateInput>,
    ) -> async_graphql::Result<PullRequestObject> {
        let st = store(ctx)?;
        let repository = st.get_repo(&org, &repo).await.map_err(core_error)?;
        authorize_write(st, &repository, viewer(ctx)).await?;

        if title.is_some() || body.is_some() || base.is_some() {
            st.update_pr(
                repository.id,
                number,
                &UpdatePr {
                    title,
                    body,
                    base,
                    state: None,
                    set_milestone: false,
                    milestone_id: None,
                },
            )
            .await
            .map_err(core_error)?;
        }

        match state {
            Some(PrStateInput::Closed) => {
                st.close_pr(repository.id, number)
                    .await
                    .map_err(core_error)?;
            }
            Some(PrStateInput::Open) => {
                st.reopen_pr(repository.id, number)
                    .await
                    .map_err(core_error)?;
            }
            None => {}
        }

        let pr = st.get_pr(repository.id, number).await.map_err(core_error)?;
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
        let author = author_id(viewer(ctx))?;

        let issue = st
            .get_issue(repository.id, number)
            .await
            .map_err(core_error)?;
        let comment = st
            .add_issue_comment(issue.id, Some(author), &body)
            .await
            .map_err(core_error)?;
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
        let author = author_id(viewer(ctx))?;

        let pr = st.get_pr(repository.id, number).await.map_err(core_error)?;
        let comment = st
            .add_pr_comment(pr.id, Some(author), &body)
            .await
            .map_err(core_error)?;
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

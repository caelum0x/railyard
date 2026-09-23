//! GraphQL output types: thin wrappers over the `rithub-core` domain, with
//! `From` conversions. Keeping the GraphQL derives here (not on the domain
//! structs) preserves the rule that `rithub-core` owns no transport concerns,
//! and lets the API hide internal fields (e.g. a repository's on-disk path).

use async_graphql::{Enum, SimpleObject};
use time::OffsetDateTime;
use uuid::Uuid;

use rithub_core::{
    Issue, IssueComment, IssueState, Milestone, MilestoneWithProgress, Organization, PrComment,
    PrState, PullRequest, Repository, Role, User, Visibility,
};

/// Repository visibility.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum VisibilityEnum {
    Public,
    Private,
}

impl From<Visibility> for VisibilityEnum {
    fn from(v: Visibility) -> Self {
        match v {
            Visibility::Public => VisibilityEnum::Public,
            Visibility::Private => VisibilityEnum::Private,
        }
    }
}

/// Open/closed state of an issue.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum IssueStateEnum {
    Open,
    Closed,
}

impl From<IssueState> for IssueStateEnum {
    fn from(s: IssueState) -> Self {
        match s {
            IssueState::Open => IssueStateEnum::Open,
            IssueState::Closed => IssueStateEnum::Closed,
        }
    }
}

impl From<IssueStateEnum> for IssueState {
    fn from(s: IssueStateEnum) -> Self {
        match s {
            IssueStateEnum::Open => IssueState::Open,
            IssueStateEnum::Closed => IssueState::Closed,
        }
    }
}

/// Pull-request lifecycle state.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum PrStateEnum {
    Open,
    Closed,
    Merged,
}

impl From<PrState> for PrStateEnum {
    fn from(s: PrState) -> Self {
        match s {
            PrState::Open => PrStateEnum::Open,
            PrState::Closed => PrStateEnum::Closed,
            PrState::Merged => PrStateEnum::Merged,
        }
    }
}

impl From<PrStateEnum> for PrState {
    fn from(s: PrStateEnum) -> Self {
        match s {
            PrStateEnum::Open => PrState::Open,
            PrStateEnum::Closed => PrState::Closed,
            PrStateEnum::Merged => PrState::Merged,
        }
    }
}

/// Coarse org membership role.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RoleEnum {
    Member,
    Admin,
    Owner,
}

impl From<Role> for RoleEnum {
    fn from(r: Role) -> Self {
        match r {
            Role::Member => RoleEnum::Member,
            Role::Admin => RoleEnum::Admin,
            Role::Owner => RoleEnum::Owner,
        }
    }
}

/// A forge user account.
#[derive(SimpleObject, Clone, Debug)]
pub struct UserObject {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub created_at: OffsetDateTime,
}

impl From<User> for UserObject {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            email: u.email,
            created_at: u.created_at,
        }
    }
}

/// An organization namespace.
#[derive(SimpleObject, Clone, Debug)]
pub struct OrganizationObject {
    pub id: Uuid,
    pub name: String,
    pub display_name: Option<String>,
    pub created_at: OffsetDateTime,
}

impl From<Organization> for OrganizationObject {
    fn from(o: Organization) -> Self {
        Self {
            id: o.id,
            name: o.name,
            display_name: o.display_name,
            created_at: o.created_at,
        }
    }
}

/// A hosted repository. The internal on-disk path is intentionally not exposed.
#[derive(SimpleObject, Clone, Debug)]
pub struct RepositoryObject {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub default_branch: String,
    pub visibility: VisibilityEnum,
    pub created_at: OffsetDateTime,
}

impl From<Repository> for RepositoryObject {
    fn from(r: Repository) -> Self {
        Self {
            id: r.id,
            org_id: r.org_id,
            name: r.name,
            description: r.description,
            default_branch: r.default_branch,
            visibility: r.visibility.into(),
            created_at: r.created_at,
        }
    }
}

/// An issue.
#[derive(SimpleObject, Clone, Debug)]
pub struct IssueObject {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: IssueStateEnum,
    pub author_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub closed_at: Option<OffsetDateTime>,
}

impl From<Issue> for IssueObject {
    fn from(i: Issue) -> Self {
        Self {
            id: i.id,
            repo_id: i.repo_id,
            number: i.number,
            title: i.title,
            body: i.body,
            state: i.state.into(),
            author_id: i.author_id,
            milestone_id: i.milestone_id,
            created_at: i.created_at,
            updated_at: i.updated_at,
            closed_at: i.closed_at,
        }
    }
}

/// A pull request.
#[derive(SimpleObject, Clone, Debug)]
pub struct PullRequestObject {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: PrStateEnum,
    pub author_id: Option<Uuid>,
    pub base_branch: String,
    pub head_branch: String,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub head_repo_id: Option<Uuid>,
    pub merge_commit_sha: Option<String>,
    pub milestone_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub merged_at: Option<OffsetDateTime>,
    pub merged_by: Option<Uuid>,
}

impl From<PullRequest> for PullRequestObject {
    fn from(p: PullRequest) -> Self {
        Self {
            id: p.id,
            repo_id: p.repo_id,
            number: p.number,
            title: p.title,
            body: p.body,
            state: p.state.into(),
            author_id: p.author_id,
            base_branch: p.base_branch,
            head_branch: p.head_branch,
            base_sha: p.base_sha,
            head_sha: p.head_sha,
            head_repo_id: p.head_repo_id,
            merge_commit_sha: p.merge_commit_sha,
            milestone_id: p.milestone_id,
            created_at: p.created_at,
            updated_at: p.updated_at,
            merged_at: p.merged_at,
            merged_by: p.merged_by,
        }
    }
}

/// A milestone with a live open/closed issue-count snapshot.
#[derive(SimpleObject, Clone, Debug)]
pub struct MilestoneObject {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub description: Option<String>,
    pub due_on: Option<OffsetDateTime>,
    pub closed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub open_issues: i32,
    pub closed_issues: i32,
}

impl From<MilestoneWithProgress> for MilestoneObject {
    fn from(m: MilestoneWithProgress) -> Self {
        let MilestoneWithProgress {
            milestone,
            open_issues,
            closed_issues,
        } = m;
        Self::from_parts(milestone, open_issues, closed_issues)
    }
}

impl MilestoneObject {
    fn from_parts(m: Milestone, open_issues: i64, closed_issues: i64) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            number: m.number,
            title: m.title,
            description: m.description,
            due_on: m.due_on,
            closed_at: m.closed_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            // Counts are small; a lossless narrowing to GraphQL's 32-bit Int.
            open_issues: open_issues as i32,
            closed_issues: closed_issues as i32,
        }
    }

    /// Build from a bare [`Milestone`] (e.g. a fresh update result) with zeroed
    /// progress counts when a re-fetch with counts is unnecessary.
    pub fn from_milestone(m: Milestone, open_issues: i64, closed_issues: i64) -> Self {
        Self::from_parts(m, open_issues, closed_issues)
    }
}

/// A comment on an issue.
#[derive(SimpleObject, Clone, Debug)]
pub struct IssueCommentObject {
    pub id: Uuid,
    pub issue_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: OffsetDateTime,
}

impl From<IssueComment> for IssueCommentObject {
    fn from(c: IssueComment) -> Self {
        Self {
            id: c.id,
            issue_id: c.issue_id,
            author_id: c.author_id,
            body: c.body,
            created_at: c.created_at,
        }
    }
}

/// A comment on a pull request.
#[derive(SimpleObject, Clone, Debug)]
pub struct PrCommentObject {
    pub id: Uuid,
    pub pr_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: OffsetDateTime,
}

impl From<PrComment> for PrCommentObject {
    fn from(c: PrComment) -> Self {
        Self {
            id: c.id,
            pr_id: c.pr_id,
            author_id: c.author_id,
            body: c.body,
            created_at: c.created_at,
        }
    }
}

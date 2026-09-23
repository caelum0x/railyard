//! Authorization for GraphQL resolvers. This mirrors `rithub-server::auth`
//! exactly (same effective-role resolution, same public-vs-private rules, same
//! scope checks) so the GraphQL plane can never be a weaker door than REST/git.
//!
//! - **Read** (query a repo and its issues/PRs/milestones): anonymous on public
//!   repos; `≥Read` effective role on private repos; deploy keys must be scoped
//!   to the repo; PAT scope must include `repo:read`.
//! - **Write** (mutations that create/update repo content): `≥Write` effective
//!   role; non-read-only deploy key scoped to the repo; PAT scope must include
//!   `repo:write`.

use rithub_core::{AccessStore, PgStore, Repository, RepoRole, Role, Store, Visibility};

use crate::context::Viewer;
use crate::error::{forbidden, unauthenticated};

/// The viewer's effective repo role, or `None` if no grant (or not a user).
async fn effective_role(
    store: &PgStore,
    repo: &Repository,
    viewer: &Viewer,
) -> async_graphql::Result<Option<RepoRole>> {
    let Some(user) = viewer.user() else {
        return Ok(None);
    };
    store
        .effective_role(user.id, repo.org_id, repo.id)
        .await
        .map_err(crate::error::core_error)
}

/// Authorize a read of `repo`. Fails closed.
pub async fn authorize_read(
    store: &PgStore,
    repo: &Repository,
    viewer: &Viewer,
) -> async_graphql::Result<()> {
    if let Viewer::DeployKey { repo_id, .. } = viewer {
        return if *repo_id == repo.id {
            Ok(())
        } else {
            Err(forbidden())
        };
    }

    if !viewer.has_scope("repo:read") {
        return Err(forbidden());
    }

    if repo.visibility == Visibility::Public {
        return Ok(());
    }
    require_role(store, repo, viewer, RepoRole::Read).await
}

/// `true` if the viewer may read `repo` — a non-erroring form of
/// [`authorize_read`] used to filter private repos out of list results.
pub async fn can_read(store: &PgStore, repo: &Repository, viewer: &Viewer) -> bool {
    authorize_read(store, repo, viewer).await.is_ok()
}

/// Authorize a write to `repo` (create/update issues, PRs, milestones, comments).
pub async fn authorize_write(
    store: &PgStore,
    repo: &Repository,
    viewer: &Viewer,
) -> async_graphql::Result<()> {
    match viewer {
        Viewer::DeployKey { repo_id, read_only } => {
            if *repo_id != repo.id || *read_only {
                return Err(forbidden());
            }
            return Ok(());
        }
        Viewer::User { .. } => {
            if !viewer.has_scope("repo:write") {
                return Err(forbidden());
            }
        }
        Viewer::Anonymous => return Err(unauthenticated()),
    }
    require_role(store, repo, viewer, RepoRole::Write).await
}

/// Require an authenticated user with at least `needed` effective role.
async fn require_role(
    store: &PgStore,
    repo: &Repository,
    viewer: &Viewer,
    needed: RepoRole,
) -> async_graphql::Result<()> {
    if matches!(viewer, Viewer::Anonymous) {
        return Err(unauthenticated());
    }
    match effective_role(store, repo, viewer).await? {
        Some(role) if role >= needed => Ok(()),
        _ => Err(forbidden()),
    }
}

/// Require an authenticated user who may **create repositories** in `org_id`
/// (org role admin/owner). Deploy keys can never create repositories.
pub async fn authorize_org_manage(
    store: &PgStore,
    org_id: uuid::Uuid,
    viewer: &Viewer,
) -> async_graphql::Result<()> {
    let Some(user) = viewer.user() else {
        return Err(unauthenticated());
    };
    match store
        .get_membership(user.id, org_id)
        .await
        .map_err(crate::error::core_error)?
    {
        Some(role) if Role::can_manage_repos(&role) => Ok(()),
        _ => Err(forbidden()),
    }
}

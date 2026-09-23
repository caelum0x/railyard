//! Request context: the authenticated [`Viewer`], the on-disk [`GitHost`]
//! delegate, and small helpers for pulling shared data out of an
//! [`async_graphql::Context`].
//!
//! The GraphQL schema is built once with the [`PgStore`] and an
//! `Arc<dyn GitHost>` as schema-level data. The `Viewer` is injected **per
//! request** by the transport (`rithub-server`), which authenticates exactly as
//! the REST/git endpoints do (HTTP Basic → PAT or deploy-key) and maps its
//! `Caller` onto a `Viewer` here. Anonymous requests carry [`Viewer::Anonymous`].

use std::sync::Arc;

use async_graphql::Context;
use rithub_core::{PgStore, User};

/// Who is making the GraphQL request, and with what privileges.
///
/// This mirrors `rithub-server`'s `Caller` (plus an explicit `Anonymous`
/// variant) so authorization decisions are identical to the REST/git plane
/// without this crate depending on the server.
#[derive(Clone, Debug)]
pub enum Viewer {
    /// No credentials presented.
    Anonymous,
    /// Authenticated via a personal access token.
    User {
        user: User,
        /// Scopes granted by this token. Empty = unrestricted (legacy tokens).
        scopes: Vec<String>,
    },
    /// Authenticated via a deploy-key bearer token (HTTP) or SSH public key.
    DeployKey {
        /// The repository this deploy key is restricted to.
        repo_id: uuid::Uuid,
        /// If `true` the key only allows reads; writes are rejected.
        read_only: bool,
    },
}

impl Viewer {
    /// The authenticated user, if this is a PAT caller.
    pub fn user(&self) -> Option<&User> {
        match self {
            Viewer::User { user, .. } => Some(user),
            _ => None,
        }
    }

    /// Whether this token grants the named scope (or is unrestricted).
    ///
    /// Deploy keys are always considered in-scope; their `read_only` flag is
    /// enforced separately by the authorization helpers.
    pub fn has_scope(&self, scope: &str) -> bool {
        match self {
            Viewer::DeployKey { .. } => true,
            Viewer::User { scopes, .. } => {
                scopes.is_empty() || scopes.iter().any(|s| s == scope || s == "admin")
            }
            Viewer::Anonymous => true,
        }
    }
}

/// The on-disk git-hosting operations the GraphQL mutations need, abstracted so
/// this crate stays free of git internals (and of the `rithub-git`/`rit` build).
/// `rithub-server` provides the concrete implementation over its `RepoHost`.
pub trait GitHost: Send + Sync {
    /// Initialize a bare repository on disk (idempotent) and return its absolute
    /// `disk_path`, exactly as the REST `create_repo` handler does.
    fn init_bare(&self, org: &str, repo: &str, default_branch: &str) -> Result<String, String>;

    /// Resolve a branch tip to a commit SHA, or `None` if it cannot be resolved
    /// (e.g. the branch does not exist yet). Used to pin a PR's base/head shas.
    fn resolve_branch_sha(&self, org: &str, repo: &str, branch: &str) -> Option<String>;
}

/// A shared [`GitHost`] handle stored as schema data.
pub type GitHostHandle = Arc<dyn GitHost>;

static ANONYMOUS: Viewer = Viewer::Anonymous;

/// The `Viewer` for the current request, defaulting to anonymous when the
/// transport injected none.
pub fn viewer<'a>(ctx: &'a Context<'_>) -> &'a Viewer {
    ctx.data_opt::<Viewer>().unwrap_or(&ANONYMOUS)
}

/// The shared metadata store.
pub fn store<'a>(ctx: &'a Context<'_>) -> async_graphql::Result<&'a PgStore> {
    ctx.data::<PgStore>()
}

/// The shared on-disk git host delegate.
pub fn git_host<'a>(ctx: &'a Context<'_>) -> async_graphql::Result<&'a GitHostHandle> {
    ctx.data::<GitHostHandle>()
}

/// The per-request forge service port (injected by the transport). Mutations
/// with side effects go through it; a missing handle is a server wiring error.
pub fn services<'a>(
    ctx: &'a Context<'_>,
) -> async_graphql::Result<&'a crate::services::ForgeServicesHandle> {
    ctx.data::<crate::services::ForgeServicesHandle>()
}

//! The pure forge domain model. These types are storage-agnostic value objects;
//! the [`crate::store`] layer maps them to and from Postgres rows.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A forge user account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// An organization — the owner namespace under which repositories live.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub display_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A hosted repository. `disk_path` is the absolute path of the bare git repo
/// on the durable filesystem; the git object/ref plane there is owned by `rit`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub default_branch: String,
    pub visibility: Visibility,
    pub disk_path: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Repository visibility, which gates anonymous access on the git endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Anyone may clone/fetch; only members may push.
    Public,
    /// Only members may clone/fetch or push.
    Private,
}

impl Visibility {
    /// The database/wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        }
    }

    /// Parse from a stored string, defaulting unknown values to `private`
    /// (fail-closed: an unrecognized visibility must never leak a repo).
    pub fn parse(value: &str) -> Self {
        match value {
            "public" => Visibility::Public,
            _ => Visibility::Private,
        }
    }
}

/// A personal access token record. The plaintext token is shown to the user
/// exactly once at creation and never stored — only its SHA-256 hash lives in
/// the database.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Token {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    /// Granted scopes. An empty list means the token is unrestricted (legacy /
    /// full-access behaviour). Non-empty lists restrict the token to the named
    /// scopes (e.g. `"repo:read"`, `"repo:write"`, `"admin"`).
    pub scopes: Vec<String>,
    /// Optional expiry. `None` means the token never expires.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub expires_at: Option<OffsetDateTime>,
    /// Revocation timestamp. `None` means the token is still active.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub revoked_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A deploy key: a repository-scoped credential that couples an SSH public key
/// (for SSH transport auth) with a bearer token (for HTTP Basic auth). The
/// bearer token plaintext is shown once at creation; only its hash is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeployKey {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub title: String,
    /// Base64 blob of the SSH public key (same format as `ssh_keys.key_blob`).
    pub key_blob: String,
    /// If `true` the key may only clone/fetch; pushes are rejected.
    pub read_only: bool,
    /// The user who created this deploy key, if still present.
    pub created_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Org membership with a coarse role. `forge-rbac` will later resolve this into
/// full team/repo access; F0 only distinguishes org owners from members.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Membership {
    pub id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub role: Role,
}

/// The coarse permission scale. Ordered from least to most privileged so
/// `role >= Role::Admin`-style checks read naturally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read access to org repositories.
    Member,
    /// Manage repositories within the org.
    Admin,
    /// Full control, including membership and deletion.
    Owner,
}

impl Role {
    /// The wire/database string for a role.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    /// Parse a role from its stored string, defaulting unknown values to the
    /// least-privileged role (fail-closed).
    pub fn parse(value: &str) -> Self {
        match value {
            "owner" => Role::Owner,
            "admin" => Role::Admin,
            _ => Role::Member,
        }
    }

    /// Whether this role is allowed to create/manage repositories in the org.
    pub fn can_manage_repos(&self) -> bool {
        *self >= Role::Admin
    }
}

/// Input to create an organization.
#[derive(Clone, Debug, Deserialize)]
pub struct NewOrganization {
    pub name: String,
    pub display_name: Option<String>,
}

/// Input to create a repository within an organization.
#[derive(Clone, Debug, Deserialize)]
pub struct NewRepository {
    pub name: String,
    pub description: Option<String>,
    /// Optional default branch override (defaults to `main`).
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Optional visibility (defaults to `public`).
    #[serde(default)]
    pub visibility: Option<Visibility>,
}

/// Input to update a repository. All fields are optional; absent fields unchanged.
/// `description` uses COALESCE in SQL, so sending `""` clears it.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct UpdateRepository {
    /// New repository name (slug).
    #[serde(default)]
    pub name: Option<String>,
    /// New human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// New default branch name.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// New visibility setting.
    #[serde(default)]
    pub visibility: Option<Visibility>,
}

//! Fine-grained access control: per-repo collaborators, teams, and **effective
//! role resolution** — the authorization kernel every git/API/web request calls.
//!
//! A user's effective role on a repo is the **maximum** of:
//! - their org role mapped to a repo role (owner/admin → Admin; member → Read),
//! - an explicit repo-collaborator grant,
//! - any team grant (a team they belong to that has access to the repo).
//!
//! Ordering `Read < Write < Admin` makes "≥ Write to push" checks read naturally.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

/// A per-repository role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoRole {
    Read,
    Write,
    Admin,
}

impl RepoRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            RepoRole::Read => "read",
            RepoRole::Write => "write",
            RepoRole::Admin => "admin",
        }
    }
    /// Parse a stored role, defaulting unknown values to the least privileged.
    pub fn parse(value: &str) -> Self {
        match value {
            "admin" => RepoRole::Admin,
            "write" => RepoRole::Write,
            _ => RepoRole::Read,
        }
    }
    pub fn can_write(&self) -> bool {
        *self >= RepoRole::Write
    }
    pub fn can_admin(&self) -> bool {
        *self >= RepoRole::Admin
    }
}

/// A team.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
}

/// Access-control persistence + resolution.
#[allow(async_fn_in_trait)]
pub trait AccessStore {
    /// Add or update an explicit repo collaborator.
    async fn set_collaborator(&self, repo_id: Uuid, user_id: Uuid, role: RepoRole) -> Result<()>;
    /// Remove a collaborator.
    async fn remove_collaborator(&self, repo_id: Uuid, user_id: Uuid) -> Result<()>;
    /// List (user_id, role) collaborators of a repo.
    async fn list_collaborators(&self, repo_id: Uuid) -> Result<Vec<(Uuid, RepoRole)>>;

    async fn create_team(&self, org_id: Uuid, name: &str) -> Result<Team>;
    async fn add_team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<()>;
    async fn grant_team_repo(&self, team_id: Uuid, repo_id: Uuid, role: RepoRole) -> Result<()>;
    async fn get_team(&self, org_id: Uuid, name: &str) -> Result<Team>;
    /// Team members as `(user_id, username)`, sorted by username.
    async fn list_team_members(&self, team_id: Uuid) -> Result<Vec<(Uuid, String)>>;
    /// Repositories a team can access as `(repo_id, repo_name, role)`.
    async fn list_team_repos(&self, team_id: Uuid) -> Result<Vec<(Uuid, String, RepoRole)>>;
    /// Remove a member from a team. Returns `false` if they were not a member.
    async fn remove_team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<bool>;
    /// Revoke a team's access to a repository. Returns `false` if none existed.
    async fn revoke_team_repo(&self, team_id: Uuid, repo_id: Uuid) -> Result<bool>;
    /// Delete a team (its memberships and repo grants cascade).
    async fn delete_team(&self, team_id: Uuid) -> Result<bool>;

    /// The user's **effective** role on `repo_id` (in org `org_id`), or `None`
    /// if they have no access at all.
    async fn effective_role(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Option<RepoRole>>;
}

impl AccessStore for PgStore {
    async fn set_collaborator(&self, repo_id: Uuid, user_id: Uuid, role: RepoRole) -> Result<()> {
        sqlx::query(
            "INSERT INTO repo_collaborators (repo_id, user_id, role) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(repo_id)
        .bind(user_id)
        .bind(role.as_str())
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn remove_collaborator(&self, repo_id: Uuid, user_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM repo_collaborators WHERE repo_id = $1 AND user_id = $2")
            .bind(repo_id)
            .bind(user_id)
            .execute(self.pool())
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn list_collaborators(&self, repo_id: Uuid) -> Result<Vec<(Uuid, RepoRole)>> {
        let rows = sqlx::query("SELECT user_id, role FROM repo_collaborators WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|r| (r.get("user_id"), RepoRole::parse(r.get::<String, _>("role").as_str())))
            .collect())
    }

    async fn create_team(&self, org_id: Uuid, name: &str) -> Result<Team> {
        let row = sqlx::query(
            "INSERT INTO teams (org_id, name) VALUES ($1, $2) RETURNING id, org_id, name",
        )
        .bind(org_id)
        .bind(name)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("team", name, e))?;
        Ok(Team {
            id: row.get("id"),
            org_id: row.get("org_id"),
            name: row.get("name"),
        })
    }

    async fn add_team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO team_members (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(team_id)
        .bind(user_id)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn grant_team_repo(&self, team_id: Uuid, repo_id: Uuid, role: RepoRole) -> Result<()> {
        sqlx::query(
            "INSERT INTO team_repo_access (team_id, repo_id, role) VALUES ($1, $2, $3) \
             ON CONFLICT (team_id, repo_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(team_id)
        .bind(repo_id)
        .bind(role.as_str())
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_team_members(&self, team_id: Uuid) -> Result<Vec<(Uuid, String)>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username FROM team_members tm JOIN users u ON u.id = tm.user_id \
             WHERE tm.team_id = $1 ORDER BY u.username",
        )
        .bind(team_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("username"))).collect())
    }

    async fn list_team_repos(&self, team_id: Uuid) -> Result<Vec<(Uuid, String, RepoRole)>> {
        let rows = sqlx::query(
            "SELECT r.id, r.name, t.role FROM team_repo_access t \
             JOIN repositories r ON r.id = t.repo_id WHERE t.team_id = $1 ORDER BY r.name",
        )
        .bind(team_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    r.get("name"),
                    RepoRole::parse(r.get::<String, _>("role").as_str()),
                )
            })
            .collect())
    }

    async fn remove_team_member(&self, team_id: Uuid, user_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
            .bind(team_id)
            .bind(user_id)
            .execute(self.pool())
            .await
            .map_err(map_db)?;
        Ok(result.rows_affected() > 0)
    }

    async fn revoke_team_repo(&self, team_id: Uuid, repo_id: Uuid) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM team_repo_access WHERE team_id = $1 AND repo_id = $2")
                .bind(team_id)
                .bind(repo_id)
                .execute(self.pool())
                .await
                .map_err(map_db)?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_team(&self, team_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM teams WHERE id = $1")
            .bind(team_id)
            .execute(self.pool())
            .await
            .map_err(map_db)?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_team(&self, org_id: Uuid, name: &str) -> Result<Team> {
        let row = sqlx::query("SELECT id, org_id, name FROM teams WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(name)
            .fetch_one(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("team", name, e))?;
        Ok(Team {
            id: row.get("id"),
            org_id: row.get("org_id"),
            name: row.get("name"),
        })
    }

    async fn effective_role(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Option<RepoRole>> {
        let mut best: Option<RepoRole> = None;
        let mut consider = |role: RepoRole| {
            best = Some(match best {
                Some(current) => current.max(role),
                None => role,
            });
        };

        // 1) Org role → repo role.
        if let Some(org_role) = sqlx::query_scalar::<_, String>(
            "SELECT role FROM memberships WHERE user_id = $1 AND org_id = $2",
        )
        .bind(user_id)
        .bind(org_id)
        .fetch_optional(self.pool())
        .await
        .map_err(map_db)?
        {
            match org_role.as_str() {
                "owner" | "admin" => consider(RepoRole::Admin),
                _ => consider(RepoRole::Read),
            }
        }

        // 2) Explicit repo collaborator.
        if let Some(role) = sqlx::query_scalar::<_, String>(
            "SELECT role FROM repo_collaborators WHERE repo_id = $1 AND user_id = $2",
        )
        .bind(repo_id)
        .bind(user_id)
        .fetch_optional(self.pool())
        .await
        .map_err(map_db)?
        {
            consider(RepoRole::parse(&role));
        }

        // 3) Team grants (teams the user belongs to that can access the repo).
        let team_roles = sqlx::query_scalar::<_, String>(
            "SELECT tra.role FROM team_repo_access tra \
             JOIN team_members tm ON tm.team_id = tra.team_id \
             WHERE tra.repo_id = $1 AND tm.user_id = $2",
        )
        .bind(repo_id)
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        for role in team_roles {
            consider(RepoRole::parse(&role));
        }

        Ok(best)
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("access", "*", e)
}

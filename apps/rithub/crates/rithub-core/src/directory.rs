//! Directory + management queries that power the web UI's account, org, and
//! repository-settings pages: a user's orgs, an org's members and teams, SSH key
//! records, webhook edit/delete + delivery history, release edit/delete, repo
//! delete/relocation, label listing, per-notification read-marking, and the
//! counters used by plan-quota enforcement.
//!
//! Kept as a separate trait so the core [`crate::Store`] contract stays small.

use crate::access::RepoRole;
use crate::error::{CoreError, Result};
use crate::store::PgStore;
use crate::{Label, Organization, Release, Role, Team, User, Webhook};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A registered SSH public key, with its metadata (the blob is public data).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshKeyRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub key_blob: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A partial webhook update. `None` fields are left unchanged.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct WebhookPatch {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// One recorded webhook delivery attempt sequence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub webhook_id: Uuid,
    pub event: String,
    pub success: bool,
    pub status_code: Option<i32>,
    pub attempts: i32,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn user_from_row(row: &sqlx::postgres::PgRow, prefix: &str) -> User {
    User {
        id: row.get(format!("{prefix}id").as_str()),
        username: row.get(format!("{prefix}username").as_str()),
        email: row.get(format!("{prefix}email").as_str()),
        created_at: row.get(format!("{prefix}created_at").as_str()),
    }
}

fn webhook_from_row(row: &sqlx::postgres::PgRow) -> Webhook {
    Webhook {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        url: row.get("url"),
        secret: row.get("secret"),
        events: row.get("events"),
        active: row.get("active"),
        created_at: row.get("created_at"),
    }
}

fn release_from_row(row: &sqlx::postgres::PgRow) -> Release {
    Release {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        tag: row.get("tag"),
        name: row.get("name"),
        body: row.get("body"),
        target_sha: row.get("target_sha"),
        author_id: row.get("author_id"),
        created_at: row.get("created_at"),
    }
}

/// Directory / management persistence.
#[allow(async_fn_in_trait)]
pub trait DirectoryStore {
    /// Every org the user belongs to, with their role, ordered by name.
    async fn list_user_orgs(&self, user_id: Uuid) -> Result<Vec<(Organization, Role)>>;
    /// Every member of an org with their role, ordered by username.
    async fn list_org_members(&self, org_id: Uuid) -> Result<Vec<(User, Role)>>;
    /// Remove a user's org membership. `NotFound` if they were not a member.
    async fn remove_membership(&self, user_id: Uuid, org_id: Uuid) -> Result<()>;
    /// Number of owners of an org (guards against removing the last owner).
    async fn count_org_owners(&self, org_id: Uuid) -> Result<i64>;
    /// Teams in an org, ordered by name.
    async fn list_teams(&self, org_id: Uuid) -> Result<Vec<Team>>;
    /// Explicit collaborators of a repo, resolved to users.
    async fn list_collaborator_users(&self, repo_id: Uuid) -> Result<Vec<(User, RepoRole)>>;

    /// Point a repository row at a new on-disk path (after a rename moved it).
    async fn set_repo_disk_path(&self, repo_id: Uuid, disk_path: &str) -> Result<()>;
    /// Delete a repository row (every dependent row cascades).
    async fn delete_repo(&self, repo_id: Uuid) -> Result<()>;
    /// Record that `repo_id` is a fork of `source_id`.
    async fn set_forked_from(&self, repo_id: Uuid, source_id: Uuid) -> Result<()>;
    /// Forks of a repository as `(org name, repo name, repo id)`.
    async fn list_forks(&self, source_id: Uuid) -> Result<Vec<(String, String, Uuid)>>;
    /// The `org/name` of the repo this one was forked from, if any.
    async fn fork_parent(&self, repo_id: Uuid) -> Result<Option<(String, String)>>;
    /// Count an org's private repositories (plan-quota enforcement).
    async fn count_private_repos(&self, org_id: Uuid) -> Result<i64>;
    /// Count an org's members (seat metering).
    async fn count_org_members(&self, org_id: Uuid) -> Result<i64>;

    /// A user's SSH keys with metadata, oldest first.
    async fn list_ssh_key_records(&self, user_id: Uuid) -> Result<Vec<SshKeyRecord>>;
    /// Delete one of a user's SSH keys. `NotFound` if it is not theirs.
    async fn delete_ssh_key(&self, user_id: Uuid, key_id: Uuid) -> Result<()>;

    /// Fetch a single webhook of a repo.
    async fn get_webhook(&self, repo_id: Uuid, hook_id: Uuid) -> Result<Webhook>;
    /// Apply a partial update to a webhook.
    async fn update_webhook(&self, repo_id: Uuid, hook_id: Uuid, patch: &WebhookPatch)
        -> Result<Webhook>;
    /// Delete a webhook (its delivery history cascades).
    async fn delete_webhook(&self, repo_id: Uuid, hook_id: Uuid) -> Result<()>;
    /// The most recent deliveries for a webhook, newest first.
    async fn list_webhook_deliveries(&self, hook_id: Uuid, limit: i64)
        -> Result<Vec<WebhookDelivery>>;

    /// Edit a release's title and notes.
    async fn update_release(
        &self,
        repo_id: Uuid,
        tag: &str,
        name: &str,
        body: Option<&str>,
    ) -> Result<Release>;
    /// Delete a release (asset rows cascade; the caller removes blobs).
    async fn delete_release(&self, repo_id: Uuid, tag: &str) -> Result<Release>;

    /// All labels defined in a repo, ordered by name.
    async fn list_labels(&self, repo_id: Uuid) -> Result<Vec<Label>>;

    /// Mark one of the user's notifications read.
    async fn mark_notification_read(&self, user_id: Uuid, notification_id: Uuid) -> Result<()>;
}

impl DirectoryStore for PgStore {
    async fn list_user_orgs(&self, user_id: Uuid) -> Result<Vec<(Organization, Role)>> {
        let rows = sqlx::query(
            "SELECT o.id, o.name, o.display_name, o.created_at, m.role \
             FROM memberships m JOIN organizations o ON o.id = m.org_id \
             WHERE m.user_id = $1 ORDER BY o.name",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("membership", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    Organization {
                        id: r.get("id"),
                        name: r.get("name"),
                        display_name: r.get("display_name"),
                        created_at: r.get("created_at"),
                    },
                    Role::parse(r.get::<String, _>("role").as_str()),
                )
            })
            .collect())
    }

    async fn list_org_members(&self, org_id: Uuid) -> Result<Vec<(User, Role)>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.email, u.created_at, m.role \
             FROM memberships m JOIN users u ON u.id = m.user_id \
             WHERE m.org_id = $1 ORDER BY u.username",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("membership", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    user_from_row(r, ""),
                    Role::parse(r.get::<String, _>("role").as_str()),
                )
            })
            .collect())
    }

    async fn remove_membership(&self, user_id: Uuid, org_id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM memberships WHERE user_id = $1 AND org_id = $2")
            .bind(user_id)
            .bind(org_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("membership", &user_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "membership",
                key: user_id.to_string(),
            });
        }
        Ok(())
    }

    async fn count_org_owners(&self, org_id: Uuid) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM memberships WHERE org_id = $1 AND role = 'owner'",
        )
        .bind(org_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("membership", "*", e))
    }

    async fn list_teams(&self, org_id: Uuid) -> Result<Vec<Team>> {
        let rows = sqlx::query("SELECT id, org_id, name FROM teams WHERE org_id = $1 ORDER BY name")
            .bind(org_id)
            .fetch_all(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("team", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| Team {
                id: r.get("id"),
                org_id: r.get("org_id"),
                name: r.get("name"),
            })
            .collect())
    }

    async fn list_collaborator_users(&self, repo_id: Uuid) -> Result<Vec<(User, RepoRole)>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.email, u.created_at, c.role \
             FROM repo_collaborators c JOIN users u ON u.id = c.user_id \
             WHERE c.repo_id = $1 ORDER BY u.username",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("collaborator", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    user_from_row(r, ""),
                    RepoRole::parse(r.get::<String, _>("role").as_str()),
                )
            })
            .collect())
    }

    async fn set_repo_disk_path(&self, repo_id: Uuid, disk_path: &str) -> Result<()> {
        sqlx::query("UPDATE repositories SET disk_path = $2 WHERE id = $1")
            .bind(repo_id)
            .bind(disk_path)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("repository", &repo_id.to_string(), e))?;
        Ok(())
    }

    async fn delete_repo(&self, repo_id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM repositories WHERE id = $1")
            .bind(repo_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("repository", &repo_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "repository",
                key: repo_id.to_string(),
            });
        }
        Ok(())
    }

    async fn set_forked_from(&self, repo_id: Uuid, source_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE repositories SET forked_from = $2 WHERE id = $1")
            .bind(repo_id)
            .bind(source_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("repository", &repo_id.to_string(), e))?;
        Ok(())
    }

    async fn list_forks(&self, source_id: Uuid) -> Result<Vec<(String, String, Uuid)>> {
        let rows = sqlx::query(
            "SELECT o.name AS org_name, r.name AS repo_name, r.id FROM repositories r \
             JOIN organizations o ON o.id = r.org_id WHERE r.forked_from = $1 \
             ORDER BY o.name, r.name",
        )
        .bind(source_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("repository", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get("org_name"), r.get("repo_name"), r.get("id")))
            .collect())
    }

    async fn fork_parent(&self, repo_id: Uuid) -> Result<Option<(String, String)>> {
        let row = sqlx::query(
            "SELECT o.name AS org_name, p.name AS repo_name FROM repositories r \
             JOIN repositories p ON p.id = r.forked_from \
             JOIN organizations o ON o.id = p.org_id WHERE r.id = $1",
        )
        .bind(repo_id)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("repository", &repo_id.to_string(), e))?;
        Ok(row.map(|r| (r.get("org_name"), r.get("repo_name"))))
    }

    async fn count_private_repos(&self, org_id: Uuid) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM repositories WHERE org_id = $1 AND visibility = 'private'",
        )
        .bind(org_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("repository", "*", e))
    }

    async fn count_org_members(&self, org_id: Uuid) -> Result<i64> {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM memberships WHERE org_id = $1")
            .bind(org_id)
            .fetch_one(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("membership", "*", e))
    }

    async fn list_ssh_key_records(&self, user_id: Uuid) -> Result<Vec<SshKeyRecord>> {
        let rows = sqlx::query(
            "SELECT id, user_id, title, key_blob, created_at FROM ssh_keys \
             WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("ssh_key", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| SshKeyRecord {
                id: r.get("id"),
                user_id: r.get("user_id"),
                title: r.get("title"),
                key_blob: r.get("key_blob"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn delete_ssh_key(&self, user_id: Uuid, key_id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM ssh_keys WHERE id = $1 AND user_id = $2")
            .bind(key_id)
            .bind(user_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("ssh_key", &key_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "ssh_key",
                key: key_id.to_string(),
            });
        }
        Ok(())
    }

    async fn get_webhook(&self, repo_id: Uuid, hook_id: Uuid) -> Result<Webhook> {
        let row = sqlx::query(
            "SELECT id, repo_id, url, secret, events, active, created_at FROM webhooks \
             WHERE id = $1 AND repo_id = $2",
        )
        .bind(hook_id)
        .bind(repo_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("webhook", &hook_id.to_string(), e))?;
        Ok(webhook_from_row(&row))
    }

    async fn update_webhook(
        &self,
        repo_id: Uuid,
        hook_id: Uuid,
        patch: &WebhookPatch,
    ) -> Result<Webhook> {
        let row = sqlx::query(
            "UPDATE webhooks SET \
               url = COALESCE($3, url), \
               secret = COALESCE($4, secret), \
               events = COALESCE($5, events), \
               active = COALESCE($6, active) \
             WHERE id = $1 AND repo_id = $2 \
             RETURNING id, repo_id, url, secret, events, active, created_at",
        )
        .bind(hook_id)
        .bind(repo_id)
        .bind(&patch.url)
        .bind(&patch.secret)
        .bind(&patch.events)
        .bind(patch.active)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("webhook", &hook_id.to_string(), e))?;
        Ok(webhook_from_row(&row))
    }

    async fn delete_webhook(&self, repo_id: Uuid, hook_id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM webhooks WHERE id = $1 AND repo_id = $2")
            .bind(hook_id)
            .bind(repo_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("webhook", &hook_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "webhook",
                key: hook_id.to_string(),
            });
        }
        Ok(())
    }

    async fn list_webhook_deliveries(
        &self,
        hook_id: Uuid,
        limit: i64,
    ) -> Result<Vec<WebhookDelivery>> {
        let rows = sqlx::query(
            "SELECT id, webhook_id, event, success, status_code, attempts, error, created_at \
             FROM webhook_deliveries WHERE webhook_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(hook_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("webhook_delivery", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| WebhookDelivery {
                id: r.get("id"),
                webhook_id: r.get("webhook_id"),
                event: r.get("event"),
                success: r.get("success"),
                status_code: r.get("status_code"),
                attempts: r.get("attempts"),
                error: r.get("error"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn update_release(
        &self,
        repo_id: Uuid,
        tag: &str,
        name: &str,
        body: Option<&str>,
    ) -> Result<Release> {
        let row = sqlx::query(
            "UPDATE releases SET name = $3, body = $4 WHERE repo_id = $1 AND tag = $2 \
             RETURNING id, repo_id, tag, name, body, target_sha, author_id, created_at",
        )
        .bind(repo_id)
        .bind(tag)
        .bind(name)
        .bind(body)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release", tag, e))?;
        Ok(release_from_row(&row))
    }

    async fn delete_release(&self, repo_id: Uuid, tag: &str) -> Result<Release> {
        let row = sqlx::query(
            "DELETE FROM releases WHERE repo_id = $1 AND tag = $2 \
             RETURNING id, repo_id, tag, name, body, target_sha, author_id, created_at",
        )
        .bind(repo_id)
        .bind(tag)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release", tag, e))?;
        Ok(release_from_row(&row))
    }

    async fn list_labels(&self, repo_id: Uuid) -> Result<Vec<Label>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, name, color FROM labels WHERE repo_id = $1 ORDER BY name",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("label", "*", e))?;
        Ok(rows
            .iter()
            .map(|r| Label {
                id: r.get("id"),
                repo_id: r.get("repo_id"),
                name: r.get("name"),
                color: r.get("color"),
            })
            .collect())
    }

    async fn mark_notification_read(&self, user_id: Uuid, notification_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE notifications SET read = true WHERE id = $1 AND user_id = $2")
            .bind(notification_id)
            .bind(user_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("notification", &notification_id.to_string(), e))?;
        Ok(())
    }
}

//! Persisted, runtime-editable paid-tool store.
//!
//! A Turnstile deployment's paid tools used to live only in a JSON file read at
//! startup. The [`ToolStore`] makes them operator-editable while the server
//! runs: every create/update/enable/disable/delete is a versioned write plus an
//! append-only audit entry (who, when, old and new spec) committed together.
//!
//! The store is spec-agnostic: a tool's declaration is an opaque JSON object
//! (`spec`) that the embedding server validates before writing. Core only owns
//! identity (`name`), lifecycle (`enabled`, soft `deleted`), optimistic
//! concurrency (`version`) and the audit trail.
//!
//! Deletion is SOFT: a deleted tool disappears from routing, discovery and MCP,
//! but its row (and name) is kept so historical settlements for its resource
//! still resolve and revenue per resource stays attributable. Re-creating a
//! deleted name revives the row with a bumped version.
//!
//! Replicas converge by polling [`ToolStore::revision`]: a value that strictly
//! increases on every write (the sum of all row versions).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::clock::now_ms;

/// One stored paid tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredTool {
    pub name: String,
    /// The tool declaration (validated by the server before every write).
    pub spec: Value,
    pub enabled: bool,
    pub deleted: bool,
    /// Bumped on every write; used for `If-Match` optimistic concurrency.
    pub version: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What an audited write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAction {
    Seed,
    Create,
    Update,
    Enable,
    Disable,
    Delete,
}

impl ToolAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Create => "create",
            Self::Update => "update",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Delete => "delete",
        }
    }

    #[must_use]
    pub fn parse(tag: &str) -> Option<Self> {
        Some(match tag {
            "seed" => Self::Seed,
            "create" => Self::Create,
            "update" => Self::Update,
            "enable" => Self::Enable,
            "disable" => Self::Disable,
            "delete" => Self::Delete,
            _ => return None,
        })
    }
}

/// One audit-trail row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolAuditEntry {
    pub id: i64,
    pub tool: String,
    pub action: ToolAction,
    /// Operator identity as reported by the admin client (plus origin address).
    pub actor: String,
    pub old_spec: Option<Value>,
    pub new_spec: Option<Value>,
    pub old_enabled: Option<bool>,
    pub new_enabled: Option<bool>,
    pub at_ms: i64,
}

/// Tool store failures.
#[derive(Debug, Error)]
pub enum ToolStoreError {
    #[error("tool {0} does not exist")]
    NotFound(String),
    #[error("tool {0} already exists")]
    Conflict(String),
    #[error("tool {name} was modified concurrently (current version {current})")]
    VersionConflict { name: String, current: i64 },
    #[error("tool store unavailable: {0}")]
    Storage(String),
}

/// Durable, audited paid-tool store.
#[async_trait]
pub trait ToolStore: Send + Sync {
    /// Every tool, including disabled and soft-deleted rows, ordered by name.
    async fn list(&self) -> Result<Vec<StoredTool>, ToolStoreError>;

    /// One tool (including soft-deleted), if it was ever created.
    async fn get(&self, name: &str) -> Result<Option<StoredTool>, ToolStoreError>;

    /// Strictly increases on every successful write; replicas poll it to reload.
    async fn revision(&self) -> Result<i64, ToolStoreError>;

    /// Create `name`. Fails with [`ToolStoreError::Conflict`] if a live
    /// (non-deleted) tool has that name; revives a soft-deleted row.
    async fn create(&self, name: &str, spec: &Value, actor: &str)
        -> Result<StoredTool, ToolStoreError>;

    /// Replace the spec of a live tool. `expected_version`, when given, must
    /// match the stored version (optimistic concurrency).
    async fn update(
        &self,
        name: &str,
        spec: &Value,
        expected_version: Option<i64>,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError>;

    /// Enable or disable a live tool.
    async fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError>;

    /// Soft-delete a live tool (disables it and hides it from routing).
    async fn delete(&self, name: &str, actor: &str) -> Result<StoredTool, ToolStoreError>;

    /// Insert `tools` ONLY if the store has never held a tool. Returns how many
    /// were inserted (0 when the store was already populated).
    async fn seed_if_empty(
        &self,
        tools: &[(String, Value)],
        actor: &str,
    ) -> Result<usize, ToolStoreError>;

    /// Most recent audit entries (newest first), optionally for one tool.
    async fn audit(
        &self,
        tool: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ToolAuditEntry>, ToolStoreError>;
}

#[derive(Default)]
struct MemoryState {
    tools: BTreeMap<String, StoredTool>,
    audit: Vec<ToolAuditEntry>,
}

impl MemoryState {
    fn record(
        &mut self,
        tool: &str,
        action: ToolAction,
        actor: &str,
        old: Option<&StoredTool>,
        new: &StoredTool,
    ) {
        let id = self.audit.len() as i64 + 1;
        self.audit.push(ToolAuditEntry {
            id,
            tool: tool.to_string(),
            action,
            actor: actor.to_string(),
            old_spec: old.map(|t| t.spec.clone()),
            new_spec: Some(new.spec.clone()),
            old_enabled: old.map(|t| t.enabled),
            new_enabled: Some(new.enabled),
            at_ms: new.updated_at_ms,
        });
    }

    fn live(&self, name: &str) -> Result<StoredTool, ToolStoreError> {
        match self.tools.get(name) {
            Some(tool) if !tool.deleted => Ok(tool.clone()),
            _ => Err(ToolStoreError::NotFound(name.to_string())),
        }
    }
}

/// Process-local store (single replica, lost on restart). Used when no
/// `TURNSTILE_DATABASE_URL` is configured.
#[derive(Default)]
pub struct InMemoryToolStore {
    state: Mutex<MemoryState>,
}

impl InMemoryToolStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(
        &self,
        f: impl FnOnce(&mut MemoryState) -> Result<R, ToolStoreError>,
    ) -> Result<R, ToolStoreError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| ToolStoreError::Storage("tool store lock poisoned".into()))?;
        f(&mut guard)
    }

    fn write(
        &self,
        name: &str,
        action: ToolAction,
        actor: &str,
        change: impl FnOnce(&StoredTool) -> StoredTool,
    ) -> Result<StoredTool, ToolStoreError> {
        self.with(|state| {
            let old = state.live(name)?;
            let new = change(&old);
            state.record(name, action, actor, Some(&old), &new);
            state.tools.insert(name.to_string(), new.clone());
            Ok(new)
        })
    }
}

fn bumped(old: &StoredTool) -> StoredTool {
    StoredTool {
        version: old.version + 1,
        updated_at_ms: now_ms(),
        ..old.clone()
    }
}

#[async_trait]
impl ToolStore for InMemoryToolStore {
    async fn list(&self) -> Result<Vec<StoredTool>, ToolStoreError> {
        self.with(|state| Ok(state.tools.values().cloned().collect()))
    }

    async fn get(&self, name: &str) -> Result<Option<StoredTool>, ToolStoreError> {
        self.with(|state| Ok(state.tools.get(name).cloned()))
    }

    async fn revision(&self) -> Result<i64, ToolStoreError> {
        self.with(|state| Ok(state.tools.values().map(|t| t.version).sum()))
    }

    async fn create(
        &self,
        name: &str,
        spec: &Value,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        self.with(|state| {
            let now = now_ms();
            let old = state.tools.get(name).cloned();
            let new = match &old {
                Some(existing) if !existing.deleted => {
                    return Err(ToolStoreError::Conflict(name.to_string()))
                }
                Some(existing) => StoredTool {
                    spec: spec.clone(),
                    enabled: true,
                    deleted: false,
                    version: existing.version + 1,
                    updated_at_ms: now,
                    ..existing.clone()
                },
                None => StoredTool {
                    name: name.to_string(),
                    spec: spec.clone(),
                    enabled: true,
                    deleted: false,
                    version: 1,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
            };
            state.record(name, ToolAction::Create, actor, old.as_ref(), &new);
            state.tools.insert(name.to_string(), new.clone());
            Ok(new)
        })
    }

    async fn update(
        &self,
        name: &str,
        spec: &Value,
        expected_version: Option<i64>,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        self.with(|state| {
            let old = state.live(name)?;
            if expected_version.is_some_and(|expected| expected != old.version) {
                return Err(ToolStoreError::VersionConflict {
                    name: name.to_string(),
                    current: old.version,
                });
            }
            let new = StoredTool {
                spec: spec.clone(),
                ..bumped(&old)
            };
            state.record(name, ToolAction::Update, actor, Some(&old), &new);
            state.tools.insert(name.to_string(), new.clone());
            Ok(new)
        })
    }

    async fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        let action = if enabled {
            ToolAction::Enable
        } else {
            ToolAction::Disable
        };
        self.write(name, action, actor, |old| StoredTool {
            enabled,
            ..bumped(old)
        })
    }

    async fn delete(&self, name: &str, actor: &str) -> Result<StoredTool, ToolStoreError> {
        self.write(name, ToolAction::Delete, actor, |old| StoredTool {
            enabled: false,
            deleted: true,
            ..bumped(old)
        })
    }

    async fn seed_if_empty(
        &self,
        tools: &[(String, Value)],
        actor: &str,
    ) -> Result<usize, ToolStoreError> {
        self.with(|state| {
            if !state.tools.is_empty() {
                return Ok(0);
            }
            let now = now_ms();
            for (name, spec) in tools {
                let new = StoredTool {
                    name: name.clone(),
                    spec: spec.clone(),
                    enabled: true,
                    deleted: false,
                    version: 1,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                state.record(name, ToolAction::Seed, actor, None, &new);
                state.tools.insert(name.clone(), new);
            }
            Ok(tools.len())
        })
    }

    async fn audit(
        &self,
        tool: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ToolAuditEntry>, ToolStoreError> {
        self.with(|state| {
            Ok(state
                .audit
                .iter()
                .rev()
                .filter(|entry| tool.map_or(true, |name| entry.tool == name))
                .take(limit)
                .cloned()
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn lifecycle_is_versioned_audited_and_soft_deleted() {
        let store = InMemoryToolStore::new();
        assert_eq!(store.seed_if_empty(&[("a".into(), json!({"p": 1}))], "seed").await.unwrap(), 1);
        assert_eq!(store.seed_if_empty(&[("b".into(), json!({}))], "seed").await.unwrap(), 0);
        let r0 = store.revision().await.unwrap();

        assert!(matches!(
            store.create("a", &json!({}), "op").await,
            Err(ToolStoreError::Conflict(_))
        ));
        let updated = store.update("a", &json!({"p": 2}), Some(1), "op").await.unwrap();
        assert_eq!(updated.version, 2);
        assert!(matches!(
            store.update("a", &json!({"p": 3}), Some(1), "op").await,
            Err(ToolStoreError::VersionConflict { current: 2, .. })
        ));
        assert!(store.revision().await.unwrap() > r0);

        let deleted = store.delete("a", "op").await.unwrap();
        assert!(deleted.deleted && !deleted.enabled);
        assert!(matches!(
            store.set_enabled("a", true, "op").await,
            Err(ToolStoreError::NotFound(_))
        ));
        let revived = store.create("a", &json!({"p": 4}), "op").await.unwrap();
        assert!(revived.enabled && !revived.deleted && revived.version == 4);

        let audit = store.audit(Some("a"), 10).await.unwrap();
        assert_eq!(audit.first().unwrap().action, ToolAction::Create);
        assert_eq!(audit.last().unwrap().action, ToolAction::Seed);
    }
}

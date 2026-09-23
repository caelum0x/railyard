//! Durable storage port for the runtime-editable catalog.
//!
//! The catalog an operator authors in `GAME_CATALOG_PATH` only SEEDS the store
//! on first boot. After that the persisted catalog is the source of truth and
//! the operator console edits it live. Every save carries the revision it was
//! based on (optimistic concurrency), so two operators or two server instances
//! can never silently overwrite each other's edits.

use std::sync::Mutex;

use crate::catalog::CatalogFile;
use crate::error::GameError;

/// A persisted catalog definition plus its monotonically increasing revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCatalog {
    /// The full catalog definition (products, pools, archived SKUs).
    pub file: CatalogFile,
    /// Revision of this definition; `1` after the first save.
    pub revision: u64,
}

/// Where the live catalog is persisted (Postgres in multi-instance
/// deployments, a JSON file for a single instance, memory in tests).
pub trait CatalogRepository: Send + Sync {
    /// The persisted catalog, or `None` when nothing has been saved yet (the
    /// server then seeds it from `GAME_CATALOG_PATH`).
    fn load_catalog(&self) -> Result<Option<StoredCatalog>, GameError>;

    /// Persist `file` only if the stored revision is still `expected_revision`
    /// (`0` = nothing stored yet). Returns the new revision. A mismatch is
    /// [`GameError::CatalogConflict`]: someone else edited first; reload and retry.
    fn save_catalog(&self, file: &CatalogFile, expected_revision: u64) -> Result<u64, GameError>;
}

/// In-process catalog storage (tests, and servers with no durable backend).
#[derive(Default)]
pub struct MemoryCatalog {
    inner: Mutex<Option<StoredCatalog>>,
}

impl CatalogRepository for MemoryCatalog {
    fn load_catalog(&self) -> Result<Option<StoredCatalog>, GameError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("catalog lock poisoned".into()))?
            .clone())
    }

    fn save_catalog(&self, file: &CatalogFile, expected_revision: u64) -> Result<u64, GameError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("catalog lock poisoned".into()))?;
        let current = guard.as_ref().map(|s| s.revision).unwrap_or(0);
        if current != expected_revision {
            return Err(GameError::CatalogConflict {
                expected: expected_revision,
                actual: current,
            });
        }
        let revision = current + 1;
        *guard = Some(StoredCatalog {
            file: file.clone(),
            revision,
        });
        Ok(revision)
    }
}

//! Shared application state: the metadata store and the git-hosting layer.

use rithub_core::PgStore;
use rithub_git::RepoHost;

/// State injected into every handler. Cloneable and cheap (a pool handle + a
/// path), so axum can share it across requests.
#[derive(Clone)]
pub struct AppState {
    /// Postgres-backed forge metadata store.
    pub store: PgStore,
    /// On-disk bare-repo host (the `rit` bridge).
    pub host: RepoHost,
}

impl AppState {
    /// Assemble state from its parts.
    pub fn new(store: PgStore, host: RepoHost) -> Self {
        Self { store, host }
    }
}

//! Shared application state: the metadata store, the git-hosting layer, and
//! an optional SMTP transport for outbound email.

use std::path::PathBuf;
use std::sync::Arc;

use lettre::AsyncSmtpTransport;
use lettre::Tokio1Executor;
use rithub_core::PgStore;
use rithub_git::RepoHost;
use rithub_graphql::RithubSchema;

/// SMTP configuration read from environment variables.
#[derive(Clone, Debug)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Address that appears in the `From:` header.
    pub from_address: String,
}

impl SmtpConfig {
    /// Read from env; returns `None` when any required var is absent.
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("SMTP_HOST").ok()?;
        let port = std::env::var("SMTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(587u16);
        let username = std::env::var("SMTP_USER").ok()?;
        let password = std::env::var("SMTP_PASS").ok()?;
        let from_address = std::env::var("SMTP_FROM")
            .unwrap_or_else(|_| format!("rithub@{host}"));
        Some(Self {
            host,
            port,
            username,
            password,
            from_address,
        })
    }

    /// Build a `lettre` async SMTP transport from this config.
    pub fn build_transport(
        &self,
    ) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        use lettre::transport::smtp::authentication::Credentials;

        let creds = Credentials::new(self.username.clone(), self.password.clone());
        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)
            .map_err(|e| anyhow::anyhow!("SMTP relay error: {e}"))?
            .port(self.port)
            .credentials(creds)
            .build();
        Ok(transport)
    }
}

/// State injected into every handler. Cloneable and cheap (pool handle +
/// path + optional SMTP transport handle).
#[derive(Clone)]
pub struct AppState {
    /// Postgres-backed forge metadata store.
    pub store: PgStore,
    /// On-disk bare-repo host (the `rit` bridge).
    pub host: RepoHost,
    /// SMTP transport for outbound email, if configured.
    pub mailer: Option<Arc<AsyncSmtpTransport<Tokio1Executor>>>,
    /// The durable data root ($RITHUB_DATA_ROOT). Release-asset blobs live under
    /// `<data_root>/release-assets/`; shared so handlers can resolve blob paths.
    pub data_root: Arc<PathBuf>,
    /// The forge GraphQL schema, mounted at `/graphql`. Cheap to clone (Arc
    /// inside). Shares the same store + git host as the REST/git plane.
    pub graphql: RithubSchema,
    /// The full-text search index (Tantivy), when the `tantivy` backend is
    /// active. `None` under the Postgres search backend or when disabled. Cheap
    /// to clone (Arc inside). See [`crate::search_index`].
    pub search: Option<Arc<forge_search::SearchIndex>>,
}

impl AppState {
    /// Assemble state from its parts.
    pub fn new(
        store: PgStore,
        host: RepoHost,
        mailer: Option<AsyncSmtpTransport<Tokio1Executor>>,
        data_root: PathBuf,
    ) -> Self {
        // The GraphQL schema shares the same store and on-disk git host as the
        // REST/git plane, so all three surfaces stay consistent.
        let graphql = crate::graphql::build_schema(store.clone(), host.clone());
        Self {
            store,
            host,
            mailer: mailer.map(Arc::new),
            data_root: Arc::new(data_root),
            graphql,
            search: None,
        }
    }

    /// Attach a full-text search index (Tantivy backend). Call after [`AppState::new`].
    pub fn with_search(mut self, index: forge_search::SearchIndex) -> Self {
        self.search = Some(Arc::new(index));
        self
    }

    /// Absolute directory that holds the blobs for a given release.
    pub fn release_asset_dir(&self, release_id: uuid::Uuid) -> PathBuf {
        self.data_root
            .join("release-assets")
            .join(release_id.to_string())
    }
}

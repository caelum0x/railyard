//! Shared application state: the metadata store, the git-hosting layer, and
//! an optional SMTP transport for outbound email.

use std::sync::Arc;

use lettre::AsyncSmtpTransport;
use lettre::Tokio1Executor;
use rithub_core::PgStore;
use rithub_git::RepoHost;

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
}

impl AppState {
    /// Assemble state from its parts.
    pub fn new(
        store: PgStore,
        host: RepoHost,
        mailer: Option<AsyncSmtpTransport<Tokio1Executor>>,
    ) -> Self {
        Self {
            store,
            host,
            mailer: mailer.map(Arc::new),
        }
    }
}

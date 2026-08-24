//! Bounded production Postgres pool configuration.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{store_err, VendStore};
use vend_core::VendError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostgresPoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub startup_connect_timeout: Duration,
    pub application_name: String,
}

impl Default for PostgresPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(1_800),
            startup_connect_timeout: Duration::from_secs(15),
            application_name: "vend-server".into(),
        }
    }
}

impl PostgresPoolConfig {
    pub fn validate(&self) -> Result<(), VendError> {
        if self.max_connections == 0 || self.max_connections > 100 {
            return Err(config_error(
                "Postgres max connections must be between 1 and 100",
            ));
        }
        if self.min_connections > self.max_connections {
            return Err(config_error(
                "Postgres min connections must not exceed max connections",
            ));
        }
        if self.acquire_timeout.is_zero()
            || self.idle_timeout.is_zero()
            || self.max_lifetime.is_zero()
            || self.startup_connect_timeout.is_zero()
        {
            return Err(config_error("Postgres pool timeouts must be non-zero"));
        }
        if self.idle_timeout > self.max_lifetime {
            return Err(config_error(
                "Postgres idle timeout must not exceed max lifetime",
            ));
        }
        if self.application_name.is_empty()
            || self.application_name.len() > 63
            || !self
                .application_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(config_error(
                "Postgres application name must be 1-63 ASCII letters, numbers, '.', '-' or '_'",
            ));
        }
        Ok(())
    }
}

impl VendStore {
    /// Connect using bounded pool and startup deadlines. Does not run migrations.
    pub async fn connect_with_config(
        database_url: &str,
        config: &PostgresPoolConfig,
    ) -> Result<Self, VendError> {
        config.validate()?;
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| config_error("DATABASE_URL is not a valid Postgres connection URL"))?
            .application_name(&config.application_name);
        let connect = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(config.idle_timeout)
            .max_lifetime(config.max_lifetime)
            .connect_with(options);
        let pool = tokio::time::timeout(config.startup_connect_timeout, connect)
            .await
            .map_err(|_| config_error("Postgres startup connection timed out"))?
            .map_err(store_err)?;
        Ok(VendStore { pool })
    }
}

fn config_error(message: &str) -> VendError {
    VendError::Storage(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_identifiable() {
        let config = PostgresPoolConfig::default();
        config.validate().unwrap();
        assert_eq!(config.max_connections, 10);
        assert_eq!(config.application_name, "vend-server");
    }

    #[test]
    fn rejects_invalid_pool_relationships_and_log_injection() {
        assert!(PostgresPoolConfig {
            min_connections: 11,
            ..PostgresPoolConfig::default()
        }
        .validate()
        .is_err());
        assert!(PostgresPoolConfig {
            application_name: "vend\nforged".into(),
            ..PostgresPoolConfig::default()
        }
        .validate()
        .is_err());
        assert!(PostgresPoolConfig {
            idle_timeout: Duration::from_secs(1_801),
            ..PostgresPoolConfig::default()
        }
        .validate()
        .is_err());
    }
}

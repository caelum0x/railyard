//! Strict environment parsing for Vend's hardening controls.

use std::time::Duration;

use vend_mint_client::MintClientConfig;
use vend_relay_client::RelayClientConfig;
use vend_store::PostgresPoolConfig;

use crate::security::{RuntimeLimits, DEFAULT_MAX_REQUEST_BYTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub postgres: PostgresPoolConfig,
    pub mint_http: MintClientConfig,
    pub relay_http: RelayClientConfig,
    pub limits: RuntimeLimits,
    pub readiness_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let postgres = PostgresPoolConfig {
            max_connections: integer("VEND_DATABASE_MAX_CONNECTIONS", 10, &lookup, 1, 100)?,
            min_connections: integer("VEND_DATABASE_MIN_CONNECTIONS", 1, &lookup, 0, 100)?,
            acquire_timeout: millis(
                "VEND_DATABASE_ACQUIRE_TIMEOUT_MS",
                10_000,
                &lookup,
                100,
                60_000,
            )?,
            idle_timeout: seconds(
                "VEND_DATABASE_IDLE_TIMEOUT_SECONDS",
                300,
                &lookup,
                30,
                3_600,
            )?,
            max_lifetime: seconds(
                "VEND_DATABASE_MAX_LIFETIME_SECONDS",
                1_800,
                &lookup,
                60,
                86_400,
            )?,
            startup_connect_timeout: seconds(
                "VEND_DATABASE_CONNECT_TIMEOUT_SECONDS",
                15,
                &lookup,
                1,
                60,
            )?,
            application_name: lookup("VEND_DATABASE_APPLICATION_NAME")
                .unwrap_or_else(|| "vend-server".into()),
        };
        postgres.validate().map_err(|error| error.to_string())?;
        let pool_idle = millis(
            "VEND_HTTP_POOL_IDLE_TIMEOUT_MS",
            30_000,
            &lookup,
            1,
            300_000,
        )?;
        let mint_http = MintClientConfig {
            connect_timeout: millis("VEND_MINT_CONNECT_TIMEOUT_MS", 3_000, &lookup, 1, 60_000)?,
            request_timeout: millis("VEND_MINT_REQUEST_TIMEOUT_MS", 15_000, &lookup, 1, 300_000)?,
            read_timeout: millis("VEND_MINT_READ_TIMEOUT_MS", 10_000, &lookup, 1, 300_000)?,
            pool_idle_timeout: pool_idle,
            max_idle_per_host: integer("VEND_MINT_MAX_IDLE_PER_HOST", 16, &lookup, 1, 1_024)?,
        };
        let relay_http = RelayClientConfig {
            connect_timeout: millis("VEND_RELAY_CONNECT_TIMEOUT_MS", 5_000, &lookup, 1, 60_000)?,
            request_timeout: millis(
                "VEND_RELAY_REQUEST_TIMEOUT_MS",
                120_000,
                &lookup,
                1,
                3_600_000,
            )?,
            read_timeout: millis("VEND_RELAY_IDLE_TIMEOUT_MS", 45_000, &lookup, 1, 600_000)?,
            stream_timeout: seconds("VEND_RELAY_MAX_STREAM_SECONDS", 900, &lookup, 30, 3_600)?,
            pool_idle_timeout: pool_idle,
            max_idle_per_host: integer("VEND_RELAY_MAX_IDLE_PER_HOST", 64, &lookup, 1, 4_096)?,
        };
        let limits = RuntimeLimits::new(integer(
            "VEND_MAX_REQUEST_BYTES",
            DEFAULT_MAX_REQUEST_BYTES,
            &lookup,
            1_024,
            16 * 1024 * 1024,
        )?)
        .map_err(str::to_owned)?;
        let readiness_timeout = millis(
            "VEND_DEPENDENCY_READINESS_TIMEOUT_MS",
            3_000,
            &lookup,
            1,
            30_000,
        )?;
        let shutdown_grace = seconds("VEND_SHUTDOWN_GRACE_SECONDS", 30, &lookup, 1, 300)?;
        Ok(Self {
            postgres,
            mint_http,
            relay_http,
            limits,
            readiness_timeout,
            shutdown_grace,
        })
    }
}

fn integer<T>(
    name: &str,
    default: T,
    lookup: &impl Fn(&str) -> Option<String>,
    min: T,
    max: T,
) -> Result<T, String>
where
    T: Copy + Ord + std::str::FromStr + std::fmt::Display,
{
    let value = match lookup(name) {
        Some(raw) => raw
            .parse::<T>()
            .map_err(|_| format!("{name} must be an integer"))?,
        None => default,
    };
    if value < min || value > max {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

fn millis(
    name: &str,
    default: u64,
    lookup: &impl Fn(&str) -> Option<String>,
    min: u64,
    max: u64,
) -> Result<Duration, String> {
    integer(name, default, lookup, min, max).map(Duration::from_millis)
}

fn seconds(
    name: &str,
    default: u64,
    lookup: &impl Fn(&str) -> Option<String>,
    min: u64,
    max: u64,
) -> Result<Duration, String> {
    integer(name, default, lookup, min, max).map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_are_bounded_and_streams_use_idle_deadline() {
        let config = RuntimeConfig::from_lookup(|_| None).unwrap();
        assert_eq!(config.limits.max_request_bytes, 2 * 1024 * 1024);
        assert_eq!(config.postgres.max_connections, 10);
        assert_eq!(config.postgres.min_connections, 1);
        assert_eq!(config.relay_http.read_timeout, Duration::from_secs(45));
        assert_eq!(config.relay_http.stream_timeout, Duration::from_secs(900));
        assert_eq!(config.shutdown_grace, Duration::from_secs(30));
    }

    #[test]
    fn rejects_zero_and_absurd_operator_values() {
        let values = HashMap::from([
            ("VEND_MINT_CONNECT_TIMEOUT_MS", "0"),
            ("VEND_MAX_REQUEST_BYTES", "999999999"),
        ]);
        let error = RuntimeConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
            .unwrap_err();
        assert!(error.contains("VEND_MINT_CONNECT_TIMEOUT_MS"));
    }

    #[test]
    fn rejects_invalid_database_pool_environment() {
        let values = HashMap::from([
            ("VEND_DATABASE_MAX_CONNECTIONS", "5"),
            ("VEND_DATABASE_MIN_CONNECTIONS", "6"),
        ]);
        assert!(
            RuntimeConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).is_err()
        );

        let values = HashMap::from([("VEND_DATABASE_APPLICATION_NAME", "vend\nforged")]);
        assert!(
            RuntimeConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).is_err()
        );
    }
}

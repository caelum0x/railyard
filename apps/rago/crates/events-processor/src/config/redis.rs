//! Port of `config/redis/redis.go`.
//!
//! Go uses go-redis with an `Addr` + separate `Password`/`DB`, stripping the
//! `redis://` / `rediss://` scheme from the URL. We reproduce that with the
//! `redis` crate's `ConnectionManager` (multiplexed async connection).

use redis::aio::ConnectionManager;
use redis::{ConnectionAddr, ConnectionInfo, RedisConnectionInfo};

#[derive(Debug, Clone, Default)]
pub struct RedisConfig {
    pub address: String,
    pub password: String,
    pub db: i64,
    pub use_tracer: bool,
    pub use_tls: bool,
}

impl RedisConfig {
    /// `AddressAndPort` — strip a leading `redis://` or `rediss://`.
    pub fn address_and_port(&self) -> String {
        let a = self.address.as_str();
        let a = a.strip_prefix("rediss://").unwrap_or(a);
        let a = a.strip_prefix("redis://").unwrap_or(a);
        a.to_string()
    }
}

#[derive(Clone)]
pub struct RedisDb {
    manager: ConnectionManager,
}

impl RedisDb {
    /// `NewRedisDB` — builds the client, then pings.
    pub async fn new(cfg: RedisConfig) -> std::result::Result<RedisDb, redis::RedisError> {
        let host_port = cfg.address_and_port();
        let (host, port) = split_host_port(&host_port);

        let addr = if cfg.use_tls {
            ConnectionAddr::TcpTls {
                host,
                port,
                insecure: true,
                tls_params: None,
            }
        } else {
            ConnectionAddr::Tcp(host, port)
        };

        let conn_info = ConnectionInfo {
            addr,
            redis: RedisConnectionInfo {
                db: cfg.db,
                username: None,
                password: if cfg.password.is_empty() {
                    None
                } else {
                    Some(cfg.password.clone())
                },
                protocol: redis::ProtocolVersion::RESP2,
            },
        };

        let client = redis::Client::open(conn_info)?;
        let mut manager = ConnectionManager::new(client).await?;

        // Ping to validate connectivity, like NewRedisDB does.
        redis::cmd("PING").query_async::<()>(&mut manager).await?;

        Ok(RedisDb { manager })
    }

    /// A cloneable handle to the multiplexed connection.
    pub fn conn(&self) -> ConnectionManager {
        self.manager.clone()
    }
}

fn split_host_port(host_port: &str) -> (String, u16) {
    match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(6379)),
        None => (host_port.to_string(), 6379),
    }
}

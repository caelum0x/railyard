//! Port of `config/database/database.go`.
//!
//! Go uses a pgx pool wrapped by gorm. Here we use a single sqlx `PgPool`
//! (Postgres, rustls). Queries are ported as sqlx statements in
//! `models::queries`.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct DbConfig {
    pub url: String,
    pub max_conns: u32,
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// `NewConnection`.
    pub async fn new_connection(config: DbConfig) -> std::result::Result<Database, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_conns)
            .connect(&config.url)
            .await?;
        Ok(Database { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// `Close`.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

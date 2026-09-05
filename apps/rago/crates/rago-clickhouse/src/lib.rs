//! # rago-clickhouse
//!
//! ClickHouse event-store backend for Rago, behind the same aggregator-facing
//! read scope as the Postgres store ([`rago_db`]). Events are written to the
//! `events_raw` MergeTree table; the boundary-scoped, ordered slice is read
//! back and handed to the same `rago-billing` aggregators, so ClickHouse and
//! Postgres produce identical numbers.
//!
//! Talks ClickHouse HTTP (`:8123`) via `reqwest`. The SQL is built by the pure,
//! unit-tested [`query`] module; the live path is exercised against a real
//! ClickHouse container (infra-gated).

pub mod query;

use chrono::{NaiveDateTime, Utc};
use rago_core::models::Event;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ClickHouseError {
    #[error("http error: {0}")]
    Http(String),
    #[error("clickhouse error {status}: {body}")]
    Server { status: u16, body: String },
    #[error("parse error: {0}")]
    Parse(String),
}

/// A ClickHouse HTTP client scoped to a database.
#[derive(Clone)]
pub struct ClickHouseStore {
    url: String,
    database: String,
    user: String,
    password: String,
    client: reqwest::Client,
}

impl ClickHouseStore {
    /// Build a client (e.g. `http://localhost:8123`, database `default`).
    pub fn new(
        url: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            database: database.into(),
            user: user.into(),
            password: password.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn execute(&self, sql: String) -> Result<String, ClickHouseError> {
        let resp = self
            .client
            .post(&self.url)
            .query(&[("database", self.database.as_str())])
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password)
            .body(sql)
            .send()
            .await
            .map_err(|e| ClickHouseError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| ClickHouseError::Http(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(ClickHouseError::Server { status, body });
        }
        Ok(body)
    }

    /// Create the `events_raw` table.
    pub async fn migrate(&self) -> Result<(), ClickHouseError> {
        self.execute(query::create_table_sql().to_string()).await?;
        Ok(())
    }

    /// Insert one event (`Events::CreateService` ClickHouse path).
    pub async fn insert_event(
        &self,
        event: &Event,
        external_customer_id: &str,
    ) -> Result<(), ClickHouseError> {
        let ts = event.timestamp.unwrap_or_else(|| Utc::now().naive_utc());
        let sql = query::insert_sql(
            &event.organization_id.to_string(),
            external_customer_id,
            event.external_subscription_id.as_deref().unwrap_or(""),
            &event.transaction_id,
            ts,
            &event.code,
            &event.properties,
            event.precise_total_amount_cents,
        );
        self.execute(sql).await?;
        Ok(())
    }

    /// `ClickhouseStore#events` — the boundary-scoped, ordered event slice for
    /// a subscription/metric, returned as [`Event`]s for the aggregators.
    pub async fn events_in_scope(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        code: &str,
        from: Option<NaiveDateTime>,
        to: NaiveDateTime,
    ) -> Result<Vec<Event>, ClickHouseError> {
        let sql = query::scope_select_sql(
            &organization_id.to_string(),
            external_subscription_id,
            code,
            from,
            to,
        );
        let body = self.execute(sql).await?;
        let now = Utc::now();
        let mut events = Vec::new();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value =
                serde_json::from_str(line).map_err(|e| ClickHouseError::Parse(e.to_string()))?;
            let properties: Value = row
                .get("properties")
                .and_then(Value::as_str)
                .map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            let ts = row.get("timestamp").and_then(Value::as_str).and_then(|s| {
                NaiveDateTime::parse_from_str(s.split('.').next().unwrap_or(s), "%Y-%m-%d %H:%M:%S")
                    .ok()
            });
            events.push(Event {
                id: Uuid::new_v4(),
                organization_id,
                customer_id: None,
                subscription_id: None,
                transaction_id: row
                    .get("transaction_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                code: code.to_string(),
                properties,
                external_customer_id: row
                    .get("external_customer_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                external_subscription_id: Some(external_subscription_id.to_string()),
                timestamp: ts,
                precise_total_amount_cents: row
                    .get("precise_total_amount_cents")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && *s != "\\N")
                    .and_then(|s| s.parse().ok()),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                persisted: true,
            });
        }
        Ok(events)
    }
}

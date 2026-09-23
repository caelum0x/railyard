//! Shared transactional projections for Abacus budget policy and Mint receipts.

use std::path::Path;

use abacus_core::error::AbacusError;
use abacus_core::identity::{AgentIdentity, AgentProfile, BudgetRepo};
use abacus_core::ids::{AgentId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptStore};
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

use crate::local_store::{load_agent_config, profile, ConfiguredAgent};

#[derive(Clone)]
pub struct PostgresProductStore {
    pool: PgPool,
}

impl PostgresProductStore {
    pub async fn connect(database_url: &str) -> Result<Self, AbacusError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(storage)?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    pub async fn migrate(&self) -> Result<(), AbacusError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|error| AbacusError::Storage(error.to_string()))
    }

    /// Install the declarative policy file atomically. The file remains the
    /// operator-owned input; every replica resolves the committed DB projection.
    pub async fn import_agent_config(&self, path: &Path) -> Result<(), AbacusError> {
        let agents = load_agent_config(path)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let ids: Vec<String> = agents.keys().cloned().collect();
        for configured in agents.values() {
            let json = serde_json::to_string(configured)
                .map_err(|error| AbacusError::Storage(error.to_string()))?;
            sqlx::query(
                "INSERT INTO abacus_agent_policies (agent_id, policy_json, updated_at) \
                 VALUES ($1, $2::jsonb, now()) ON CONFLICT (agent_id) DO UPDATE SET \
                 policy_json = EXCLUDED.policy_json, updated_at = now()",
            )
            .bind(configured.agent_id.as_str())
            .bind(json)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        sqlx::query("DELETE FROM abacus_agent_policies WHERE NOT (agent_id = ANY($1))")
            .bind(&ids)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)
    }
}

fn storage(error: sqlx::Error) -> AbacusError {
    AbacusError::Storage(error.to_string())
}

fn identity(configured: ConfiguredAgent, run: &RunId) -> AgentIdentity {
    AgentIdentity {
        agent_acct: configured.agent_acct,
        run_id: run.clone(),
        treasury: configured.treasury,
        payee: configured.payee,
        budget: configured.budget,
    }
}

#[async_trait]
impl BudgetRepo for PostgresProductStore {
    async fn resolve(&self, agent: &AgentId, run: &RunId) -> Result<AgentIdentity, AbacusError> {
        let row = sqlx::query(
            "SELECT policy_json::text AS policy_json FROM abacus_agent_policies WHERE agent_id = $1",
        )
        .bind(agent.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or(AbacusError::AccountNotFound)?;
        let configured: ConfiguredAgent = serde_json::from_str(row.get("policy_json"))
            .map_err(|error| AbacusError::Storage(format!("invalid stored policy: {error}")))?;
        Ok(identity(configured, run))
    }

    async fn list_agents(&self) -> Result<Vec<AgentProfile>, AbacusError> {
        let rows = sqlx::query(
            "SELECT policy_json::text AS policy_json FROM abacus_agent_policies ORDER BY agent_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let configured: ConfiguredAgent = serde_json::from_str(row.get("policy_json"))
                    .map_err(|error| {
                        AbacusError::Storage(format!("invalid stored policy: {error}"))
                    })?;
                Ok(profile(&configured))
            })
            .collect()
    }
}

#[async_trait]
impl ReceiptStore for PostgresProductStore {
    async fn record(&self, receipt: &MintReceipt) -> Result<(), AbacusError> {
        let json = serde_json::to_string(receipt)
            .map_err(|error| AbacusError::Storage(error.to_string()))?;
        let result = sqlx::query(
            "INSERT INTO abacus_receipts (hold_id, request_id, receipt_json) \
             VALUES ($1, $2, $3::jsonb) ON CONFLICT (hold_id) DO NOTHING",
        )
        .bind(&receipt.hold_id)
        .bind(&receipt.request_id)
        .bind(&json)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        match self.get(&receipt.hold_id).await? {
            Some(existing) if existing == *receipt => Ok(()),
            Some(_) => Err(AbacusError::IdempotencyConflict),
            None => Err(AbacusError::Storage(
                "receipt disappeared while recording".to_string(),
            )),
        }
    }

    async fn get(&self, hold_id: &str) -> Result<Option<MintReceipt>, AbacusError> {
        let row = sqlx::query(
            "SELECT receipt_json::text AS receipt_json FROM abacus_receipts WHERE hold_id = $1",
        )
        .bind(hold_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.map(|row| {
            serde_json::from_str(row.get("receipt_json"))
                .map_err(|error| AbacusError::Storage(format!("invalid stored receipt: {error}")))
        })
        .transpose()
    }

    async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MintReceipt>, AbacusError> {
        // `request_id` is UNIQUE in the schema, so this resolves to at most one
        // settled receipt for the correlation id.
        let row = sqlx::query(
            "SELECT receipt_json::text AS receipt_json FROM abacus_receipts \
             WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.map(|row| {
            serde_json::from_str(row.get("receipt_json"))
                .map_err(|error| AbacusError::Storage(format!("invalid stored receipt: {error}")))
        })
        .transpose()
    }

    async fn list_recent(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MintReceipt>, AbacusError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT receipt_json::text AS receipt_json FROM abacus_receipts \
             WHERE ($1::text IS NULL OR receipt_json->>'agent_id' = $1) \
             ORDER BY created_at DESC, hold_id DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| {
                serde_json::from_str(row.get("receipt_json")).map_err(|error| {
                    AbacusError::Storage(format!("invalid stored receipt: {error}"))
                })
            })
            .collect()
    }
}

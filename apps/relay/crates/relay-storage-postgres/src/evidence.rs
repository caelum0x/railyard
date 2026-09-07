//! Postgres-backed settlement evidence for Relay.

use async_trait::async_trait;
use relay_billing::{AccountId, BillingError, JobId, SettlementEvidence, SettlementEvidenceStore};
use sqlx::PgPool;

#[derive(Clone)]
pub struct PostgresSettlementEvidenceStore {
    pool: PgPool,
}

impl PostgresSettlementEvidenceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SettlementEvidenceStore for PostgresSettlementEvidenceStore {
    async fn record(&self, evidence: &SettlementEvidence) -> Result<(), BillingError> {
        let receipt = serde_json::to_value(&evidence.receipt)
            .map_err(|error| BillingError::Store(format!("encode settlement receipt: {error}")))?;
        let attestation = evidence
            .attestation
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| {
                BillingError::Store(format!("encode settlement attestation: {error}"))
            })?;
        let result = sqlx::query(
            "INSERT INTO relay_settlement_evidence \
             (principal, request_id, receipt, attestation, verified_at_ms, settled_at_ms) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (principal, request_id) DO NOTHING",
        )
        .bind(evidence.principal.as_str())
        .bind(evidence.request_id.as_uuid())
        .bind(receipt)
        .bind(attestation)
        .bind(evidence.verified_at_ms)
        .bind(evidence.settled_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| BillingError::Store(format!("write settlement evidence: {error}")))?;

        if result.rows_affected() == 0 {
            let existing = self
                .get(&evidence.principal, &evidence.request_id)
                .await?
                .ok_or_else(|| {
                    BillingError::Store(
                        "settlement evidence conflict disappeared during lookup".into(),
                    )
                })?;
            if existing.receipt != evidence.receipt || existing.attestation != evidence.attestation
            {
                return Err(BillingError::Store(
                    "conflicting settlement evidence for principal/request".into(),
                ));
            }
        }
        Ok(())
    }

    async fn get(
        &self,
        principal: &AccountId,
        request_id: &JobId,
    ) -> Result<Option<SettlementEvidence>, BillingError> {
        let row: Option<(serde_json::Value, Option<serde_json::Value>, Option<i64>, i64)> = sqlx::query_as(
            "SELECT receipt, attestation, verified_at_ms, settled_at_ms FROM relay_settlement_evidence \
             WHERE principal = $1 AND request_id = $2",
        )
        .bind(principal.as_str())
        .bind(request_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| BillingError::Store(format!("read settlement evidence: {error}")))?;

        row.map(|(receipt, attestation, verified_at_ms, settled_at_ms)| {
            let receipt = serde_json::from_value(receipt).map_err(|error| {
                BillingError::Store(format!("decode settlement receipt: {error}"))
            })?;
            let attestation = attestation
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| {
                    BillingError::Store(format!("decode settlement attestation: {error}"))
                })?;
            Ok(SettlementEvidence {
                request_id: *request_id,
                principal: principal.clone(),
                receipt,
                attestation,
                verified_at_ms,
                settled_at_ms,
            })
        })
        .transpose()
    }
}

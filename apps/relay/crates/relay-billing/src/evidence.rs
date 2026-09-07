//! Durable settlement-evidence vocabulary shared by Relay's billing edge and
//! downstream callers that need to prove a request actually settled.
//!
//! The lookup key is deliberately `(principal, request_id)`: a request UUID is
//! not a bearer token and cannot be used to inspect another payer's receipt.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{AccountId, BillingError, JobId};
use aitp_client::{HoldSettlementAttestation, Receipt};

/// Portable signed-receipt vocabulary shared with Passport, Abacus and
/// Ledgerline. The Mint wire receipt uses terse keys; evidence APIs do not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementReceiptProof {
    #[serde(alias = "seq")]
    pub sequence: u64,
    #[serde(alias = "op")]
    pub operation: String,
    #[serde(alias = "op_hash")]
    pub operation_hash: String,
    pub amount_micros: i64,
    /// Preserved from Mint so Relay can reproduce the original signed receipt
    /// header on a settled replay. Absent on evidence written before v1.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_after_micros: Option<i64>,
    /// Preserved hash-chain predecessor; absent on legacy evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
    #[serde(alias = "hash")]
    pub receipt_hash: String,
    #[serde(alias = "sig")]
    pub signature: String,
}

impl From<Receipt> for SettlementReceiptProof {
    fn from(receipt: Receipt) -> Self {
        Self {
            sequence: receipt.seq,
            operation: receipt.op,
            operation_hash: receipt.op_hash,
            amount_micros: receipt.amount_micros.get(),
            balance_after_micros: Some(receipt.balance_after_micros.get()),
            prev_hash: Some(receipt.prev_hash),
            receipt_hash: receipt.hash,
            signature: receipt.sig,
        }
    }
}

/// Signed Mint evidence for one successfully settled Relay request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementEvidence {
    pub request_id: JobId,
    pub principal: AccountId,
    pub receipt: SettlementReceiptProof,
    /// Full independently verified Mint hold-settlement attestation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<HoldSettlementAttestation>,
    /// Relay wall-clock time at which the pinned-key verification succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at_ms: Option<i64>,
    pub settled_at_ms: i64,
}

/// Append/read seam for settlement evidence.
///
/// Implementations must make `record` idempotent for the composite identity.
/// Re-recording a byte-equivalent receipt is a no-op; conflicting evidence is
/// an error rather than an overwrite of the audit trail.
#[async_trait]
pub trait SettlementEvidenceStore: Send + Sync {
    async fn record(&self, evidence: &SettlementEvidence) -> Result<(), BillingError>;

    async fn get(
        &self,
        principal: &AccountId,
        request_id: &JobId,
    ) -> Result<Option<SettlementEvidence>, BillingError>;
}

/// Process-local implementation for unit tests and explicitly non-durable
/// development. Production wiring uses the Postgres implementation in gateway.
#[derive(Default)]
pub struct InMemorySettlementEvidenceStore {
    records: std::sync::RwLock<std::collections::HashMap<(AccountId, JobId), SettlementEvidence>>,
}

#[async_trait]
impl SettlementEvidenceStore for InMemorySettlementEvidenceStore {
    async fn record(&self, evidence: &SettlementEvidence) -> Result<(), BillingError> {
        let key = (evidence.principal.clone(), evidence.request_id);
        let mut records = self
            .records
            .write()
            .map_err(|_| BillingError::Store("settlement evidence lock poisoned".into()))?;
        match records.get(&key) {
            Some(existing)
                if existing.receipt != evidence.receipt
                    || existing.attestation != evidence.attestation =>
            {
                Err(BillingError::Store(
                    "conflicting settlement evidence for principal/request".into(),
                ))
            }
            Some(_) => Ok(()),
            None => {
                records.insert(key, evidence.clone());
                Ok(())
            }
        }
    }

    async fn get(
        &self,
        principal: &AccountId,
        request_id: &JobId,
    ) -> Result<Option<SettlementEvidence>, BillingError> {
        self.records
            .read()
            .map_err(|_| BillingError::Store("settlement evidence lock poisoned".into()))
            .map(|records| records.get(&(principal.clone(), *request_id)).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitp_client::AmountMicros;

    fn evidence(hash: &str) -> SettlementEvidence {
        SettlementEvidence {
            request_id: JobId::parse("018f3c20-c57a-7a4b-8f7e-1ed8e42b63c0").unwrap(),
            principal: AccountId::new("acct_owner"),
            receipt: SettlementReceiptProof {
                sequence: 7,
                operation: "hold.capture".into(),
                operation_hash: "op".into(),
                amount_micros: AmountMicros::new(42).get(),
                balance_after_micros: None,
                prev_hash: None,
                receipt_hash: hash.into(),
                signature: "sig".into(),
            },
            attestation: None,
            verified_at_ms: None,
            settled_at_ms: 123,
        }
    }

    #[tokio::test]
    async fn lookup_is_principal_scoped_and_replay_safe() {
        let store = InMemorySettlementEvidenceStore::default();
        let row = evidence("hash");
        store.record(&row).await.unwrap();
        store.record(&row).await.unwrap();
        assert_eq!(
            store.get(&row.principal, &row.request_id).await.unwrap(),
            Some(row.clone())
        );
        assert!(store
            .get(&AccountId::new("acct_other"), &row.request_id)
            .await
            .unwrap()
            .is_none());
        assert!(store.record(&evidence("different")).await.is_err());
    }
}

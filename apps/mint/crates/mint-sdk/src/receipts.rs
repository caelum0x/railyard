//! Signed account receipt-chain export and global Merkle audit roots.

use serde::Deserialize;

use crate::{Error, MintClient, TransferReceiptBody};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SignedReceipt {
    pub body: ReceiptBody,
    pub hash: String,
    pub sig: String,
}

pub type ReceiptBody = TransferReceiptBody;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AuditProof {
    pub merkle_root: String,
    pub epoch: u64,
}

impl MintClient {
    pub async fn account_receipts(&self, account_id: &str) -> Result<Vec<SignedReceipt>, Error> {
        self.get(&format!("v1/accounts/{account_id}/receipts"))
            .await
    }

    pub async fn receipt_audit(&self) -> Result<AuditProof, Error> {
        self.get("v1/receipts/audit").await
    }
}

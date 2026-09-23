//! Shared test doubles for the injectable Relay/Mint seams. The in-memory store
//! lives in `passport-store`; this crate provides the transport fakes so server
//! and service tests run with no network.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use passport_core::{
    MintMeter, MintMeterRequest, MintMeterResult, PassportError, PaymentEvidence,
    RelayConditioningSink, RelayGenerationRequest, RelayGenerationResult, SettlementReceiptProof,
};

/// A Relay fake that echoes a deterministic output ref and counts calls.
#[derive(Default)]
pub struct FakeRelay {
    pub calls: AtomicUsize,
}

impl FakeRelay {
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RelayConditioningSink for FakeRelay {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RelayGenerationResult {
            output_ref: format!(
                "fake://{}/{}/{}/{n}",
                request.provider, request.modality, request.passport_id
            ),
            asset_hash: Some(format!("sha256:fake{n}")),
            payment_evidence: Some(PaymentEvidence {
                request_id: format!("req_{n}"),
                payer_account: request.principal.clone(),
                receipt: SettlementReceiptProof {
                    sequence: n as u64 + 1,
                    operation: "hold.capture".into(),
                    operation_hash: format!("op_hash_{n}"),
                    amount_micros: 1_000,
                    receipt_hash: format!("receipt_hash_{n}"),
                    signature: format!("signed_receipt_{n}"),
                },
            }),
            output_text: Some(format!(
                "fake output {n}: {}",
                request.prompt.clone().unwrap_or_default()
            )),
        })
    }
}

/// A Relay fake that models Relay's own request idempotency: every call returns
/// the SAME request-correlated, settled response (same `request_id`, output, and
/// receipt), so a retried generation must dedupe to a single provenance record.
/// Counts calls so a test can prove Relay was hit twice yet only one record was
/// written.
#[derive(Default)]
pub struct ReplayRelay {
    pub calls: AtomicUsize,
}

impl ReplayRelay {
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RelayConditioningSink for ReplayRelay {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RelayGenerationResult {
            output_ref: format!(
                "fake://{}/{}/{}",
                request.provider, request.modality, request.passport_id
            ),
            asset_hash: Some("sha256:stable".into()),
            payment_evidence: Some(PaymentEvidence {
                request_id: "req_stable".into(),
                payer_account: request.principal.clone(),
                receipt: SettlementReceiptProof {
                    sequence: 1,
                    operation: "hold.capture".into(),
                    operation_hash: "op_hash_stable".into(),
                    amount_micros: 1_000,
                    receipt_hash: "receipt_hash_stable".into(),
                    signature: "signed_receipt_stable".into(),
                },
            }),
            output_text: Some("stable output".into()),
        })
    }
}

/// A Mint fake that returns a deterministic receipt and counts calls.
#[derive(Default)]
pub struct FakeMint {
    pub calls: AtomicUsize,
}

impl FakeMint {
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MintMeter for FakeMint {
    async fn meter(&self, request: &MintMeterRequest) -> Result<MintMeterResult, PassportError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(MintMeterResult {
            receipt_ref: Some(format!("rcpt_{}_{n}", request.reference)),
        })
    }
}

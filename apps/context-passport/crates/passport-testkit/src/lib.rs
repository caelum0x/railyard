//! Shared test doubles for the injectable Relay/Mint seams. The in-memory store
//! lives in `passport-store`; this crate provides the transport fakes so server
//! and service tests run with no network.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use passport_core::{
    MintMeter, MintMeterRequest, MintMeterResult, PassportError, RelayConditioningSink,
    RelayGenerationRequest, RelayGenerationResult,
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

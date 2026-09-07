//! A no-crypto, no-network stub gateway used in every test and until Mint's
//! AITP-402 (AP3) is real.
//!
//! It always authorizes with a canned, dummy-signed authorization, echoes the
//! challenge fields, mints synthetic holds/receipts, and records every call so
//! tests can assert what the runtime asked for. It can also be configured to
//! fail authorization so the reject path is exercisable offline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::amount::AmountMicros;
use crate::error::PayError;
use crate::gateway::{Authorized, PaymentGateway};
use crate::types::{HoldRef, PaymentAuthorization, PaymentChallenge, Receipt};

/// Recorded interactions, for test assertions.
#[derive(Debug, Default)]
pub struct StubCalls {
    pub authorized: Vec<PaymentChallenge>,
    pub captured: Vec<(HoldRef, AmountMicros)>,
    pub released: Vec<HoldRef>,
}

/// Deterministic fake [`PaymentGateway`].
pub struct StubPaymentGateway {
    payer: String,
    /// When set, `authorize` returns this error instead of succeeding — lets a
    /// test drive the pay-and-reject path (e.g. `InsufficientFunds`).
    fail_authorize_with: Option<fn() -> PayError>,
    seq: AtomicU64,
    calls: Mutex<StubCalls>,
}

impl Default for StubPaymentGateway {
    fn default() -> Self {
        Self::new("acct_stub_payer")
    }
}

impl StubPaymentGateway {
    /// A gateway that always authorizes, paying from `payer`.
    pub fn new(payer: impl Into<String>) -> Self {
        Self {
            payer: payer.into(),
            fail_authorize_with: None,
            seq: AtomicU64::new(1),
            calls: Mutex::new(StubCalls::default()),
        }
    }

    /// A gateway whose `authorize` always fails with `make_err()` — for the
    /// reject path.
    pub fn always_failing(make_err: fn() -> PayError) -> Self {
        Self {
            fail_authorize_with: Some(make_err),
            ..Self::default()
        }
    }

    /// Snapshot-friendly access to recorded calls.
    pub fn with_calls<R>(&self, f: impl FnOnce(&StubCalls) -> R) -> R {
        f(&self.calls.lock().expect("stub call log poisoned"))
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl PaymentGateway for StubPaymentGateway {
    async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError> {
        self.calls
            .lock()
            .expect("stub call log poisoned")
            .authorized
            .push(challenge.clone());

        if let Some(make_err) = self.fail_authorize_with {
            return Err(make_err());
        }

        let idempotency_key = format!("req_stub_{}", self.next_seq());
        let authorization = PaymentAuthorization {
            payer: self.payer.clone(),
            payee: challenge.payee.clone(),
            amount_micros: challenge.amount_micros,
            nonce: challenge.nonce.clone(),
            resource: challenge.resource.clone(),
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key: idempotency_key.clone(),
            pubkey_id: "key_stub".to_string(),
            op_hash: format!("stub_ophash_{}", challenge.nonce),
            sig: format!("stub_sig_{}", challenge.nonce),
        };

        let hold = Some(HoldRef(format!("hold_stub_{idempotency_key}")));
        Ok(Authorized {
            authorization,
            hold,
        })
    }

    async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError> {
        self.calls
            .lock()
            .expect("stub call log poisoned")
            .captured
            .push((hold.clone(), actual));

        let seq = self.next_seq();
        Ok(Receipt {
            seq,
            op: "hold.capture".to_string(),
            op_hash: format!("stub_ophash_capture_{seq}"),
            amount_micros: actual,
            balance_after_micros: AmountMicros::ZERO,
            prev_hash: "stub_prev".to_string(),
            hash: format!("stub_hash_{seq}"),
            sig: "stub_verifier_sig".to_string(),
        })
    }

    async fn release(&self, hold: &HoldRef) -> Result<(), PayError> {
        self.calls
            .lock()
            .expect("stub call log poisoned")
            .released
            .push(hold.clone());
        Ok(())
    }
}

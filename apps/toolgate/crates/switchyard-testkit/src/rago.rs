use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use switchyard_meter::{RagoError, RagoTransport};

/// Records events; `fail_next` makes the next N deliveries fail (retryable).
#[derive(Default)]
pub struct FakeRago {
    pub events: Mutex<Vec<Value>>,
    pub fail_next: AtomicUsize,
}

impl FakeRago {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(FakeRago::default())
    }
    pub fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl RagoTransport for FakeRago {
    async fn send_event(&self, event: &Value) -> Result<(), RagoError> {
        if self.fail_next.load(Ordering::SeqCst) > 0 {
            self.fail_next.fetch_sub(1, Ordering::SeqCst);
            return Err(RagoError { retryable: true, message: "fake rago down".into() });
        }
        let mut ev = self.events.lock().unwrap();
        let tx = event.get("transaction_id").cloned();
        if tx.is_some() && ev.iter().any(|e| e.get("transaction_id") == tx.as_ref()) {
            return Ok(()); // idempotent on transaction_id
        }
        ev.push(event.clone());
        Ok(())
    }
}

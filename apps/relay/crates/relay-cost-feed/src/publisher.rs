use async_trait::async_trait;

use crate::{CostEvent, Result};

#[async_trait]
pub trait CostEventPublisher: Send + Sync {
    /// Publishers must use `event_id` as their idempotency key because delivery
    /// is deliberately at least once.
    async fn publish(&self, event: &CostEvent) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPublisher;

#[async_trait]
impl CostEventPublisher for NoopPublisher {
    async fn publish(&self, _event: &CostEvent) -> Result<()> {
        Ok(())
    }
}

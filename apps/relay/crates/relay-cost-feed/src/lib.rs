//! Durable, at-least-once cost events emitted only after successful settlement.

mod error;
mod model;
mod publisher;
mod store;
mod worker;

pub use error::{CostFeedError, Result};
pub use model::{CostEvent, CostEventValidationError, InferenceRoute, METERED_RECEIPT_PREFIX};
pub use publisher::{CostEventPublisher, NoopPublisher};
pub use store::{
    CostEventListQuery, CostEventPage, CostEventState, CostEventStore, DEFAULT_LIST_LIMIT,
    InMemoryCostEventStore, MAX_LIST_LIMIT, PendingCostEvent,
};
pub use worker::{CostFeedClock, CostFeedWorker, DrainReport, SystemCostFeedClock, WorkerPolicy};

pub const COST_EVENT_SCHEMA_VERSION: u16 = 1;

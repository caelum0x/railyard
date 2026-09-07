use thiserror::Error;

pub type Result<T> = std::result::Result<T, CostFeedError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CostFeedError {
    #[error("cost event is invalid: {0}")]
    InvalidEvent(String),
    #[error("cost event conflicts with an existing event id")]
    Conflict,
    #[error("cost event was not found")]
    NotFound,
    #[error("cost event is not a replayable dead letter")]
    NotReplayable,
    #[error("cost event query is invalid: {0}")]
    InvalidQuery(String),
    #[error("cost event lease is not owned by this worker")]
    LeaseOwnership,
    #[error("cost event store failed: {0}")]
    Store(String),
    #[error("cost event publisher failed: {0}")]
    Publish(String),
    #[error("cost feed arithmetic overflow")]
    Overflow,
}

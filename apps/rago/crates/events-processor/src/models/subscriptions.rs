//! Port of `models/subscriptions.go` (struct + JSON shape).
//!
//! The `ApiStore::fetch_subscription` and `get_all_subscriptions` query methods
//! live in `models/stores.rs` and `models/queries.rs` respectively, alongside
//! the other sqlx-backed queries.

use serde::{Deserialize, Serialize};

use crate::utils::time::NullTime;

/// Subscription bucket duration, in seconds (`SUBSCRIPTION_BUCKET_DURATION`).
pub const SUBSCRIPTION_BUCKET_DURATION: i64 = 10;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub organization_id: Option<String>,
    pub external_id: String,
    pub plan_id: String,
    #[serde(default)]
    pub created_at: NullTime,
    #[serde(default)]
    pub updated_at: NullTime,
    #[serde(default)]
    pub started_at: NullTime,
    #[serde(default)]
    pub terminated_at: NullTime,
}

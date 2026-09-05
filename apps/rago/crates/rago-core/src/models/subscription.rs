use crate::enums::{BillingTime, SubscriptionStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `subscriptions` — a customer's subscription to a plan. (Lago
/// `app/models/subscription.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub plan_id: Uuid,
    pub billing_entity_id: Option<Uuid>,
    pub external_id: String,
    pub name: Option<String>,
    pub status: SubscriptionStatus,
    pub billing_time: BillingTime,
    pub subscription_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ending_at: Option<DateTime<Utc>>,
    pub trial_ended_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
    pub terminated_at: Option<DateTime<Utc>>,
    pub previous_subscription_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Subscription {
    pub fn is_active(&self) -> bool {
        matches!(self.status, SubscriptionStatus::Active)
    }
}

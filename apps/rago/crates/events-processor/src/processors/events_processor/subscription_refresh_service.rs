//! Port of `processors/events_processor/subscription_refresh_service.go`.

use std::sync::Arc;

use crate::models::{EnrichedEvent, Flagger};
use crate::utils::result::Result;
use crate::utils::RagoError;

pub struct SubscriptionRefreshService {
    flag_store: Arc<dyn Flagger>,
}

impl SubscriptionRefreshService {
    pub fn new(flag_store: Arc<dyn Flagger>) -> SubscriptionRefreshService {
        SubscriptionRefreshService { flag_store }
    }

    /// `FlagSubscriptionRefresh`.
    pub async fn flag_subscription_refresh(&self, event: &EnrichedEvent) -> Result<bool> {
        let value = format!("{}:{}", event.organization_id, event.subscription_id);
        match self.flag_store.flag(&value).await {
            Ok(()) => Result::success(true),
            Err(e) => Result::failed(RagoError::Message(e.to_string())),
        }
    }
}

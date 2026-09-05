//! Port of `processors/events_processor/cache_service.go`.

use crate::models::{ChargeCache, EnrichedEvent};
use crate::utils::error_tracker;

pub struct CacheService {
    charge_cache_store: ChargeCache,
}

impl CacheService {
    pub fn new(charge_cache_store: ChargeCache) -> CacheService {
        CacheService { charge_cache_store }
    }

    /// `ExpireCache`.
    pub async fn expire_cache(&self, events: &[EnrichedEvent]) {
        for event in events {
            let flat_filter = match &event.flat_filter {
                Some(f) => f,
                None => continue,
            };

            let cache_result = self
                .charge_cache_store
                .expire(flat_filter, &event.subscription_id)
                .await;
            if cache_result.is_failure() {
                if let Some(err) = cache_result.error() {
                    error_tracker::capture_error(err);
                }
            }
        }
    }
}

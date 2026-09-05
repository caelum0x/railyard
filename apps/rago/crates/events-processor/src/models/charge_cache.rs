//! Port of `models/charge_cache.go`.
//!
//! The RFC3339 timestamps in the key are formatted at second precision with a
//! `Z` suffix, matching Go's `time.RFC3339` on a UTC time. This key MUST match
//! the usage reader's key exactly, so the formatting is load-bearing.

use std::sync::Arc;

use crate::utils::result::Result;

use super::flat_filters::FlatFilter;
use super::stores::Cacher;

pub const CACHE_KEY_VERSION: &str = "1";

/// `time.RFC3339` for a UTC time: `2006-01-02T15:04:05Z` (no fractional part).
const RFC3339_SECS_Z: &str = "%Y-%m-%dT%H:%M:%SZ";

#[derive(Clone)]
pub struct ChargeCache {
    pub cache_store: Arc<dyn Cacher>,
}

impl ChargeCache {
    pub fn new(cache_store: Arc<dyn Cacher>) -> ChargeCache {
        ChargeCache { cache_store }
    }

    /// `(*ChargeCache).Expire`.
    pub async fn expire(&self, ff: &FlatFilter, sub_id: &str) -> Result<bool> {
        let mut key_parts: Vec<String> = vec![
            "charge-usage".to_string(),
            CACHE_KEY_VERSION.to_string(),
            ff.charge_id.clone(),
            sub_id.to_string(),
            ff.charge_updated_at.format(RFC3339_SECS_Z).to_string(),
        ];

        if let (Some(cf_id), Some(cf_updated)) = (
            ff.charge_filter_id.as_ref(),
            ff.charge_filter_updated_at.as_ref(),
        ) {
            key_parts.push(cf_id.clone());
            key_parts.push(cf_updated.format(RFC3339_SECS_Z).to_string());
        }

        let cache_key = key_parts.join("/");
        self.cache_store.expire_key(&cache_key).await
    }
}

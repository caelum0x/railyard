//! Port of `cache/subscriptions.go` — keying, best-match search and TTL delete.

use chrono::{DateTime, Utc};

use crate::models::Subscription;
use crate::utils::result::Result;
use crate::utils::RagoError;

use super::store::Cache;

pub const SUBSCRIPTION_PREFIX: &str = "sub";
pub const SUBSCRIPTION_MODEL_NAME: &str = "subscriptions";
pub const SUBSCRIPTION_TOPIC: &str = ".public.subscriptions";

impl Cache {
    pub fn build_subscription_key(
        &self,
        organization_id: &str,
        external_id: &str,
        id: &str,
    ) -> String {
        format!("{SUBSCRIPTION_PREFIX}:{organization_id}:{external_id}:{id}")
    }

    pub fn subscription_key(&self, sub: &Subscription) -> std::result::Result<String, RagoError> {
        match &sub.organization_id {
            None => Err(RagoError::Message(format!(
                "subscription {} has nil OrganizationID",
                sub.id
            ))),
            Some(org) => Ok(self.build_subscription_key(org, &sub.external_id, &sub.id)),
        }
    }

    pub fn set_subscription(&self, sub: &Subscription) -> Result<bool> {
        match self.subscription_key(sub) {
            Ok(key) => self.set_json(&key, sub),
            Err(e) => Result::failed(e),
        }
    }

    pub fn get_subscription(
        &self,
        organization_id: &str,
        external_id: &str,
        id: &str,
    ) -> Result<Subscription> {
        let key = self.build_subscription_key(organization_id, external_id, id);
        self.get_json::<Subscription>(&key)
    }

    /// `SearchSubscriptions` — reproduces the Postgres
    /// `ORDER BY terminated_at DESC NULLS FIRST, started_at DESC` selection.
    pub fn search_subscriptions(
        &self,
        organization_id: &str,
        external_id: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<Subscription> {
        let prefix = format!("{SUBSCRIPTION_PREFIX}:{organization_id}:{external_id}:");
        let result = self.search_json::<Subscription>(&prefix);
        if result.is_failure() {
            return Result::failed(result.into_error().unwrap());
        }
        let subscriptions = result.into_value();

        let mut valid_subs: Vec<Subscription> = Vec::new();
        for sub in subscriptions {
            if !sub.started_at.valid() {
                continue;
            }
            if sub.started_at.time() > timestamp {
                continue;
            }
            // Not terminated, or terminated at/after the timestamp.
            if !sub.terminated_at.valid()
                || sub.terminated_at.time() > timestamp
                || sub.terminated_at.time() == timestamp
            {
                valid_subs.push(sub);
            }
        }

        let mut best: Option<Subscription> = None;
        for sub in valid_subs.into_iter() {
            let b = match &best {
                None => {
                    best = Some(sub);
                    continue;
                }
                Some(b) => b,
            };

            // NULL FIRST for terminated_at.
            if !b.terminated_at.valid() && sub.terminated_at.valid() {
                continue;
            }
            if b.terminated_at.valid() && !sub.terminated_at.valid() {
                best = Some(sub);
                continue;
            }

            if b.terminated_at.valid() && sub.terminated_at.valid() {
                if sub.terminated_at.time() > b.terminated_at.time() {
                    best = Some(sub);
                    continue;
                }
                if sub.terminated_at.time() < b.terminated_at.time() {
                    continue;
                }
            }

            // terminated_at equal (or both NULL) -> started_at DESC.
            if sub.started_at.valid()
                && b.started_at.valid()
                && sub.started_at.time() > b.started_at.time()
            {
                best = Some(sub);
            }
        }

        match best {
            Some(b) => Result::success(b),
            None => Result::failed(RagoError::RecordNotFound)
                .non_capturable()
                .non_retryable(),
        }
    }

    /// `DeleteSubscription` — kept for 30 days to allow grace-period backfill.
    pub fn delete_subscription(&self, sub: &Subscription) -> Result<bool> {
        match self.subscription_key(sub) {
            Ok(key) => self.set_json_with_ttl(&key, sub, chrono::Duration::days(30)),
            Err(e) => Result::failed(e),
        }
    }
}

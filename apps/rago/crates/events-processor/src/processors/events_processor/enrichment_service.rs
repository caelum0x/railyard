//! Port of `processors/events_processor/enrichment_service.go`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::cache::Cache;
use crate::expression;
use crate::models::billable_metrics::AGGREGATION_TYPE_COUNT;
use crate::models::{
    matching_filter, ApiStore, BillableMetric, EnrichedEvent, Event, FlatFilter, Subscription,
    HTTP_RUBY, TARGET_WALLET_CODE,
};
use crate::utils::result::{failed_from, Result};
use crate::utils::string::go_sprintf_v;
use crate::utils::RagoError;

pub struct EventEnrichmentService {
    api_store: Option<ApiStore>,
    mem_cache: Option<Arc<Cache>>,
}

impl EventEnrichmentService {
    pub fn new(
        api_store: Option<ApiStore>,
        mem_cache: Option<Arc<Cache>>,
    ) -> EventEnrichmentService {
        EventEnrichmentService {
            api_store,
            mem_cache,
        }
    }

    /// `EnrichEvent`.
    pub async fn enrich_event(&self, event: &Arc<Event>) -> Result<Vec<EnrichedEvent>> {
        let enriched_result = event.to_enriched_event();
        if enriched_result.is_failure() {
            return failed_from(
                &enriched_result,
                "build_enriched_event",
                "Error while converting event to enriched event",
            );
        }
        let mut enriched = enriched_result.into_value();

        // --- billable metric ---
        let bm_result = if let Some(cache) = &self.mem_cache {
            cache.get_billable_metric(&event.organization_id, &event.code)
        } else {
            self.api_store
                .as_ref()
                .unwrap()
                .fetch_billable_metric(&event.organization_id, &event.code)
                .await
        };
        if bm_result.is_failure() {
            return failed_from(
                &bm_result,
                "fetch_billable_metric",
                "Error fetching billable metric",
            );
        }
        let bm = Arc::new(bm_result.into_value());

        let enrich_bm = self.enrich_with_billable_metric(&mut enriched, &bm);
        if enrich_bm.is_failure() {
            return failed_from(
                &enrich_bm,
                enrich_bm.error_code(),
                enrich_bm.error_message(),
            );
        }

        // --- subscription ---
        let mut sub_result = self.fetch_subscription(event, enriched.time).await;

        // Recurring metrics fall back on the currently-active subscription.
        if sub_result.is_failure() && !sub_result.is_capturable() && bm.recurring {
            sub_result = self.fetch_subscription(event, Utc::now()).await;
        }

        let sub: Option<Subscription> = if sub_result.is_failure() {
            if sub_result.is_capturable() {
                return failed_from(
                    &sub_result,
                    "fetch_subscription",
                    "Error fetching subscription",
                );
            }
            None
        } else {
            Some(sub_result.into_value())
        };

        if let Some(sub) = sub {
            self.enrich_with_subscription(&mut enriched, Arc::new(sub));
        }

        self.enrich_with_charge_info(enriched).await
    }

    async fn fetch_subscription(
        &self,
        event: &Event,
        timestamp: DateTime<Utc>,
    ) -> Result<Subscription> {
        if let Some(cache) = &self.mem_cache {
            cache.search_subscriptions(
                &event.organization_id,
                &event.external_subscription_id,
                timestamp,
            )
        } else {
            self.api_store
                .as_ref()
                .unwrap()
                .fetch_subscription(
                    &event.organization_id,
                    &event.external_subscription_id,
                    timestamp,
                )
                .await
        }
    }

    /// `enrichWithBillableMetric` — mutates `enriched` in place; the returned
    /// `Result<()>` only carries success/failure and error details.
    fn enrich_with_billable_metric(
        &self,
        enriched: &mut EnrichedEvent,
        bm: &Arc<BillableMetric>,
    ) -> Result<()> {
        enriched.billable_metric = Some(bm.clone());
        enriched.aggregation_type = bm.aggregation_type.to_string();

        if enriched.source != HTTP_RUBY {
            let expr_result = self.evaluate_expression(enriched, bm);
            if expr_result.is_failure() {
                return failed_from(
                    &expr_result,
                    "evaluate_expression",
                    "Error evaluating custom expression",
                );
            }
        }

        if bm.aggregation_type == AGGREGATION_TYPE_COUNT {
            enriched.value = Some("1".to_string());
        } else {
            let raw = enriched
                .properties
                .get(&bm.field_name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            enriched.value = Some(go_sprintf_v(&raw));
        }

        Result::success(())
    }

    /// `evaluateExpression`.
    fn evaluate_expression(&self, ev: &mut EnrichedEvent, bm: &BillableMetric) -> Result<bool> {
        if bm.expression.is_empty() {
            return Result::success(false);
        }

        let event_json = match serde_json::to_string(&*ev) {
            Ok(j) => j,
            Err(e) => return Result::<bool>::failed(RagoError::from(e)).non_retryable(),
        };

        match expression::evaluate(&bm.expression, &event_json) {
            Some(result) => {
                ev.properties
                    .insert(bm.field_name.clone(), serde_json::Value::String(result));
                Result::success(true)
            }
            None => Result::<bool>::failed(RagoError::Message(format!(
                "failed to evaluate expr: {} with json: {}",
                bm.expression, event_json
            )))
            .non_retryable(),
        }
    }

    /// `enrichWithSubscription`.
    fn enrich_with_subscription(&self, enriched: &mut EnrichedEvent, sub: Arc<Subscription>) {
        enriched.subscription_id = sub.id.clone();
        enriched.plan_id = sub.plan_id.clone();
        enriched.subscription = Some(sub);
    }

    /// `enrichWithChargeInfo`.
    async fn enrich_with_charge_info(&self, enriched: EnrichedEvent) -> Result<Vec<EnrichedEvent>> {
        if enriched.subscription.is_none() {
            return Result::success(vec![enriched]);
        }

        let filters_result = if let Some(cache) = &self.mem_cache {
            cache.build_flat_filters(&enriched.organization_id, &enriched.code, &enriched.plan_id)
        } else {
            self.api_store
                .as_ref()
                .unwrap()
                .fetch_flat_filters(&enriched.organization_id, &enriched.plan_id, &enriched.code)
                .await
        };
        if filters_result.is_failure() {
            return Result::failed(filters_result.into_error().unwrap());
        }

        let filters = filters_result.into_value();
        if filters.is_empty() {
            return Result::success(vec![enriched]);
        }

        // Index filters by charge id (an event can match multiple charges).
        let mut order: Vec<String> = Vec::new();
        let mut charges: HashMap<String, Vec<FlatFilter>> = HashMap::new();
        for filter in filters {
            if !charges.contains_key(&filter.charge_id) {
                order.push(filter.charge_id.clone());
            }
            charges
                .entry(filter.charge_id.clone())
                .or_default()
                .push(filter);
        }

        let mut enriched_events: Vec<EnrichedEvent> = Vec::new();
        for charge_id in order {
            let charge_filters = &charges[&charge_id];
            let matching = matching_filter(charge_filters, &enriched);

            let mut copy = enriched.clone();
            copy.grouped_by = Default::default();

            copy.charge_id = Some(matching.charge_id.clone());
            copy.charge_updated_at = Some(matching.charge_updated_at);
            copy.charge_filter_id = matching.charge_filter_id.clone();
            copy.charge_filter_updated_at = matching.charge_filter_updated_at;
            copy.flat_filter = Some(matching);

            enrich_with_pricing_group_keys(&mut copy);
            enriched_events.push(copy);
        }

        Result::success(enriched_events)
    }
}

/// `enrichWithPricingGroupKeys`.
fn enrich_with_pricing_group_keys(event: &mut EnrichedEvent) {
    let flat_filter = match event.flat_filter.clone() {
        Some(f) => f,
        None => return,
    };

    for key in &flat_filter.pricing_group_keys {
        match event.properties.get(key) {
            Some(v) if !v.is_null() => {
                event.grouped_by.insert(key.clone(), go_sprintf_v(v));
            }
            _ => {
                event.grouped_by.insert(key.clone(), String::new());
            }
        }
    }

    if flat_filter.accepts_target_wallet {
        if let Some(raw) = event.properties.get(TARGET_WALLET_CODE) {
            if !raw.is_null() {
                let formatted = go_sprintf_v(raw);
                event
                    .grouped_by
                    .insert(TARGET_WALLET_CODE.to_string(), formatted.clone());
                event.target_wallet_code = Some(formatted);
            }
        }
    }
}

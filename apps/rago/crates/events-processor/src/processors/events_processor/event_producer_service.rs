//! Port of `processors/events_processor/event_producer_service.go`.

use std::sync::Arc;

use chrono::Utc;

use crate::config::kafka::{MessageProducer, ProducerMessage};
use crate::models::{EnrichedEvent, Event, FailedEvent};
use crate::utils::error_tracker;
use crate::utils::result::AnyResult;
use crate::utils::RagoError;

pub struct EventProducerService {
    enriched_producer: Arc<dyn MessageProducer>,
    enriched_expanded_producer: Arc<dyn MessageProducer>,
    in_advance_producer: Arc<dyn MessageProducer>,
    dead_letter_producer: Arc<dyn MessageProducer>,
}

impl EventProducerService {
    pub fn new(
        enriched_producer: Arc<dyn MessageProducer>,
        enriched_expanded_producer: Arc<dyn MessageProducer>,
        in_advance_producer: Arc<dyn MessageProducer>,
        dead_letter_producer: Arc<dyn MessageProducer>,
    ) -> EventProducerService {
        EventProducerService {
            enriched_producer,
            enriched_expanded_producer,
            in_advance_producer,
            dead_letter_producer,
        }
    }

    fn msg_key(event: &EnrichedEvent) -> Vec<u8> {
        format!("{}-{}", event.organization_id, event.transaction_id).into_bytes()
    }

    pub async fn produce_enriched_event(&self, event: &EnrichedEvent) {
        let key = Self::msg_key(event);
        if let Err(_e) = self
            .produce_event(event, key, self.enriched_producer.clone())
            .await
        {
            tracing::error!("error while marshaling enriched events");
        }
    }

    pub async fn produce_enriched_expanded_event(&self, event: &EnrichedEvent) {
        let key = Self::msg_key(event);
        if let Err(_e) = self
            .produce_event(event, key, self.enriched_expanded_producer.clone())
            .await
        {
            tracing::error!("error while marshaling enriched expended events");
        }
    }

    pub async fn produce_charged_in_advance_event(&self, event: &EnrichedEvent) {
        let key = Self::msg_key(event);
        if let Err(_e) = self
            .produce_event(event, key, self.in_advance_producer.clone())
            .await
        {
            tracing::error!("error while marshaling charged in advance events");
        }
    }

    /// `ProduceToDeadLetterQueue`.
    pub async fn produce_to_dead_letter_queue(&self, event: Event, error_result: &dyn AnyResult) {
        let failed_event = FailedEvent {
            initial_error_message: error_result.error_msg(),
            error_code: error_result.error_code(),
            error_message: error_result.error_message(),
            failed_at: Utc::now(),
            event: event.clone(),
        };

        let event_json = match serde_json::to_vec(&failed_event) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("error while marshaling failed event with error details");
                error_tracker::capture_error(&RagoError::from(e));
                Vec::new()
            }
        };

        let pushed = self
            .dead_letter_producer
            .produce(&ProducerMessage {
                key: None,
                value: event_json,
            })
            .await;

        if !pushed {
            tracing::error!(topic = %self.dead_letter_producer.get_topic(), "error while pushing to dead letter topic");
            let extra = serde_json::to_value(&event).ok();
            error_tracker::capture_error_result_with_extra(error_result, "event", extra);
        }
    }

    /// `produceEvent`.
    async fn produce_event(
        &self,
        event: &EnrichedEvent,
        key: Vec<u8>,
        producer: Arc<dyn MessageProducer>,
    ) -> std::result::Result<(), RagoError> {
        let event_json = serde_json::to_vec(event).map_err(RagoError::from)?;

        let pushed = producer
            .produce(&ProducerMessage {
                key: Some(key),
                value: event_json,
            })
            .await;

        if !pushed {
            if let Some(initial) = &event.initial_event {
                let err = crate::utils::result::failed_bool(RagoError::Message(format!(
                    "failed to push to {} topic",
                    producer.get_topic()
                )));
                self.produce_to_dead_letter_queue((**initial).clone(), &err)
                    .await;
            }
        }

        Ok(())
    }
}

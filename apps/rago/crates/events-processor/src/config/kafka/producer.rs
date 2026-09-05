//! Port of `config/kafka/producer.go` — a synchronous-ish producer over
//! rdkafka's `FutureProducer` (franz-go `ProduceSync` → awaited `send`).

use std::time::Duration;

use async_trait::async_trait;
use rdkafka::producer::{FutureProducer, FutureRecord};

use crate::utils::error_tracker;
use crate::utils::RagoError;

use super::client::ServerConfig;
use super::record::ProducerMessage;

#[derive(Debug, Clone)]
pub struct ProducerConfig {
    pub topic: String,
}

/// `MessageProducer` interface.
#[async_trait]
pub trait MessageProducer: Send + Sync {
    async fn produce(&self, msg: &ProducerMessage) -> bool;
    fn get_topic(&self) -> &str;
}

pub struct Producer {
    client: FutureProducer,
    config: ProducerConfig,
}

impl Producer {
    /// `NewProducer`.
    pub fn new(
        server_config: &ServerConfig,
        cfg: ProducerConfig,
    ) -> std::result::Result<Producer, rdkafka::error::KafkaError> {
        let client: FutureProducer = server_config.base_client_config().create()?;
        Ok(Producer {
            client,
            config: cfg,
        })
    }

    /// `Ping` — resolve broker metadata to validate connectivity.
    pub fn ping(&self) -> std::result::Result<(), rdkafka::error::KafkaError> {
        use rdkafka::producer::Producer as _;
        self.client
            .client()
            .fetch_metadata(None, Duration::from_secs(5))
            .map(|_| ())
    }
}

#[async_trait]
impl MessageProducer for Producer {
    /// `(*Producer).Produce`.
    async fn produce(&self, msg: &ProducerMessage) -> bool {
        let span = crate::config::tracing::start_span("Producer.Produce");

        let empty_key: Vec<u8> = Vec::new();
        let key = msg.key.as_ref().unwrap_or(&empty_key);
        let record = FutureRecord::to(&self.config.topic)
            .payload(&msg.value)
            .key(key);

        let result = self.client.send(record, Duration::from_secs(0)).await;
        span.end();

        match result {
            Ok(_) => true,
            Err((err, _)) => {
                tracing::error!(
                    error = %err,
                    "record had a produce error while synchronously producing"
                );
                error_tracker::capture_error(&RagoError::Message(err.to_string()));
                false
            }
        }
    }

    fn get_topic(&self) -> &str {
        &self.config.topic
    }
}

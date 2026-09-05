//! Port of the Go `config/kafka` package (franz-go → rdkafka).

pub mod client;
pub mod consumer;
pub mod producer;
pub mod record;

pub use client::{ServerConfig, SCRAM_256, SCRAM_512};
pub use consumer::{
    find_max_commitable_record, ConsumerGroup, ConsumerGroupConfig, ProcessRecordsFn,
};
pub use producer::{MessageProducer, Producer, ProducerConfig};
pub use record::{ProducerMessage, Record};

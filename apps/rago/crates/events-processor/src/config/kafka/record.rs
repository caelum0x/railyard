//! An owned Kafka record, decoupled from the underlying client so the
//! processing pipeline and the commit-prefix logic are testable in isolation.
//! Mirrors the fields of `kgo.Record` the Go service relies on.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
}

impl Record {
    /// The `string(key)-offset` identity Go uses when matching processed records.
    pub(crate) fn identity(&self) -> String {
        let key = self.key.as_deref().unwrap_or(&[]);
        format!("{}-{}", String::from_utf8_lossy(key), self.offset)
    }
}

/// A message to produce (`ProducerMessage`).
#[derive(Debug, Clone, Default)]
pub struct ProducerMessage {
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

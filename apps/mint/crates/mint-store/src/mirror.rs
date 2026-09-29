//! The portfolio mirror (Wave P PY1.5): one record per paid event a product
//! (Scribase, Menivor, Rally) received from its payment provider, kept next to
//! the ledger but never in it. A mirror record moves no balance, holds no
//! funds and forwards nothing: Mint never initiates a payout of anyone's money
//! (tested in mint-server). Each record is fanned out to the configured
//! product endpoints as a signed outbound webhook with retries.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use mint_core::MintError;

/// What a mirrored event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorKind {
    Payment,
    Refund,
    Dispute,
}

impl MirrorKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "payment" => Some(Self::Payment),
            "refund" => Some(Self::Refund),
            "dispute" => Some(Self::Dispute),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Payment => "payment",
            Self::Refund => "refund",
            Self::Dispute => "dispute",
        }
    }
}

/// One mirrored paid event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorEvent {
    /// Mint's id (`mev_<ulid>`).
    pub id: String,
    pub product: String,
    pub provider: String,
    /// The provider's own event or payment id; unique per provider.
    pub provider_event_id: String,
    pub kind: MirrorKind,
    /// Whole micro-units of `currency` (a record, not a balance).
    pub amount_micros: i64,
    pub currency: String,
    pub customer_ref: String,
    pub occurred_at_ms: i64,
    pub recorded_at_ms: i64,
}

/// A pending outbound delivery of one event to one endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorDelivery {
    pub event: MirrorEvent,
    pub endpoint: String,
    pub attempts: i32,
}

/// Storage for the mirror and its outbox.
#[async_trait]
pub trait MirrorStore: Send + Sync {
    /// Records the event and one delivery row per endpoint. `false` when the
    /// provider event was recorded before (a replay: nothing changes).
    async fn record(&self, event: MirrorEvent, endpoints: &[String]) -> Result<bool, MintError>;
    /// Events of one product, newest first.
    async fn list(&self, product: &str, limit: usize) -> Result<Vec<MirrorEvent>, MintError>;
    /// Deliveries due at `now_ms`, oldest first.
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<MirrorDelivery>, MintError>;
    async fn delivered(&self, event_id: &str, endpoint: &str, now_ms: i64) -> Result<(), MintError>;
    /// Schedules the next attempt at `next_ms` and records why this one failed.
    async fn failed(&self, event_id: &str, endpoint: &str, next_ms: i64, error: &str) -> Result<(), MintError>;
}

/// One delivery row: (attempts, next attempt ms, delivered ms, last error).
type DeliveryRow = (i32, i64, Option<i64>, Option<String>);

#[derive(Default)]
struct MemoryInner {
    events: Vec<MirrorEvent>,
    /// (event id, endpoint) -> delivery row.
    deliveries: BTreeMap<(String, String), DeliveryRow>,
}

/// In-memory mirror (tests and `MINT_ALLOW_INMEMORY`).
#[derive(Default)]
pub struct MemoryMirror {
    inner: Mutex<MemoryInner>,
}

impl MemoryMirror {
    /// Delivered timestamp of one delivery (tests).
    pub fn delivered_at(&self, event_id: &str, endpoint: &str) -> Option<i64> {
        let inner = self.inner.lock().ok()?;
        inner.deliveries.get(&(event_id.to_owned(), endpoint.to_owned())).and_then(|d| d.2)
    }
}

#[async_trait]
impl MirrorStore for MemoryMirror {
    async fn record(&self, event: MirrorEvent, endpoints: &[String]) -> Result<bool, MintError> {
        let mut inner = self.inner.lock().map_err(|_| MintError::Storage)?;
        if inner
            .events
            .iter()
            .any(|e| e.provider == event.provider && e.provider_event_id == event.provider_event_id)
        {
            return Ok(false);
        }
        for endpoint in endpoints {
            inner
                .deliveries
                .insert((event.id.clone(), endpoint.clone()), (0, event.recorded_at_ms, None, None));
        }
        inner.events.push(event);
        Ok(true)
    }

    async fn list(&self, product: &str, limit: usize) -> Result<Vec<MirrorEvent>, MintError> {
        let inner = self.inner.lock().map_err(|_| MintError::Storage)?;
        let mut out: Vec<MirrorEvent> = inner.events.iter().filter(|e| e.product == product).cloned().collect();
        out.sort_by(|a, b| b.recorded_at_ms.cmp(&a.recorded_at_ms).then(b.id.cmp(&a.id)));
        out.truncate(limit);
        Ok(out)
    }

    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<MirrorDelivery>, MintError> {
        let inner = self.inner.lock().map_err(|_| MintError::Storage)?;
        let mut out = Vec::new();
        for ((event_id, endpoint), (attempts, next_ms, delivered, _)) in &inner.deliveries {
            if delivered.is_none() && *next_ms <= now_ms {
                if let Some(event) = inner.events.iter().find(|e| &e.id == event_id) {
                    out.push(MirrorDelivery { event: event.clone(), endpoint: endpoint.clone(), attempts: *attempts });
                }
            }
        }
        out.sort_by_key(|d| d.event.recorded_at_ms);
        out.truncate(limit);
        Ok(out)
    }

    async fn delivered(&self, event_id: &str, endpoint: &str, now_ms: i64) -> Result<(), MintError> {
        let mut inner = self.inner.lock().map_err(|_| MintError::Storage)?;
        if let Some(d) = inner.deliveries.get_mut(&(event_id.to_owned(), endpoint.to_owned())) {
            d.0 += 1;
            d.2 = Some(now_ms);
            d.3 = None;
        }
        Ok(())
    }

    async fn failed(&self, event_id: &str, endpoint: &str, next_ms: i64, error: &str) -> Result<(), MintError> {
        let mut inner = self.inner.lock().map_err(|_| MintError::Storage)?;
        if let Some(d) = inner.deliveries.get_mut(&(event_id.to_owned(), endpoint.to_owned())) {
            d.0 += 1;
            d.1 = next_ms;
            d.3 = Some(error.chars().take(300).collect());
        }
        Ok(())
    }
}

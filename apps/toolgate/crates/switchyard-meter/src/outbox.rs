//! Durable outbox drain: rows written atomically with the call record are
//! delivered here, retried with exponential backoff, never lost on a crash.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use switchyard_store::Store;

use crate::rago::RagoTransport;

pub struct OutboxWorker {
    pub store: Arc<dyn Store>,
    pub rago: Option<Arc<dyn RagoTransport>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub delivered: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl OutboxWorker {
    fn backoff(attempts: i32) -> chrono::Duration {
        let secs = 2i64.saturating_pow(attempts.clamp(0, 12) as u32).min(3600);
        chrono::Duration::seconds(secs)
    }

    /// Deliver every due row once.
    pub async fn drain_once(&self) -> DrainReport {
        let mut report = DrainReport::default();
        let due = match self.store.due_outbox(Utc::now(), 100).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "outbox read failed");
                return report;
            }
        };
        for item in due {
            let result = match (item.kind.as_str(), &self.rago) {
                ("rago_event", Some(rago)) => rago.send_event(&item.payload).await.map_err(|e| e.message),
                ("rago_event", None) => {
                    // Rago not configured: keep the row for when it is.
                    report.skipped += 1;
                    let _ = self.store.mark_outbox(&item.id, false, Some("rago not configured".into()), Utc::now() + chrono::Duration::hours(1)).await;
                    continue;
                }
                (other, _) => Err(format!("unknown outbox kind {other}")),
            };
            match result {
                Ok(()) => {
                    report.delivered += 1;
                    let _ = self.store.mark_outbox(&item.id, true, None, Utc::now()).await;
                }
                Err(message) => {
                    report.failed += 1;
                    tracing::warn!(id = %item.id, error = %message, "outbox delivery failed");
                    let _ = self.store.mark_outbox(&item.id, false, Some(message), Utc::now() + Self::backoff(item.attempts + 1)).await;
                }
            }
        }
        report
    }

    /// Run forever (spawned by the server).
    pub async fn run(self: Arc<Self>, every: Duration) {
        loop {
            self.drain_once().await;
            tokio::time::sleep(every).await;
        }
    }
}

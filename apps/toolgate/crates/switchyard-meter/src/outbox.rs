//! Durable outbox drain: rows written atomically with the call record are
//! delivered here, retried with exponential backoff, never lost on a crash.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use switchyard_store::Store;

use crate::mint::{MintError, MintTransport};
use crate::rago::RagoTransport;

/// Delivers `rago_event`, `mint_capture` and `mint_release` rows. Mint rows
/// exist only when a settlement could not reach Mint during the call; they
/// replay with the same idempotency key, so a retry can never double-charge.
pub struct OutboxWorker {
    pub store: Arc<dyn Store>,
    pub rago: Option<Arc<dyn RagoTransport>>,
    pub mint: Option<Arc<dyn MintTransport>>,
}

fn mint_result(r: Result<crate::mint::MintReceipt, MintError>) -> Result<(), String> {
    match r {
        Ok(_) | Err(MintError::HoldNotFound) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
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
                ("mint_capture", _) => {
                    let p = &item.payload;
                    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let amount = p.get("amount_micros").and_then(|v| v.as_i64()).unwrap_or(0);
                    match &self.mint {
                        Some(m) => mint_result(m.capture(&s("hold_id"), amount, &s("to"), &s("idem_key")).await),
                        None => Err("mint not configured".to_string()),
                    }
                }
                ("mint_release", _) => {
                    let p = &item.payload;
                    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    match &self.mint {
                        Some(m) => mint_result(m.release(&s("hold_id"), &s("idem_key")).await),
                        None => Err("mint not configured".to_string()),
                    }
                }
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

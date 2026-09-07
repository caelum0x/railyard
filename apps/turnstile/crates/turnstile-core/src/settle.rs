//! Success-only hold settlement backed by a replayable command journal.
//!
//! `Settled` never owns the only copy of a Mint hold. The journal receives the
//! terminal direction before Mint I/O, so capture ambiguity can only replay
//! capture and handler failure can only replay release. Dropping the handle
//! stops its lease heartbeat; the recovery worker claims and releases the
//! expired held command instead of relying on asynchronous destructor I/O.

use std::sync::Arc;
use std::time::Duration;

use aitp_client::{AmountMicros, Receipt, Scheme};
use tokio::sync::oneshot;

use crate::clock::now_ms;
use crate::error::TurnstileError;
use crate::journal::{SettlementJournal, SettlementPhase, SettlementRecord};
use crate::verifier::PaymentVerifier;

/// A verified authorization whose handler lease owns the next state change.
pub struct Settled {
    verifier: Arc<dyn PaymentVerifier>,
    journal: Arc<dyn SettlementJournal>,
    nonce: String,
    owner_token: String,
    hold: Option<aitp_client::HoldRef>,
    scheme: Scheme,
    quoted: AmountMicros,
    lease: Duration,
    stop_heartbeat: Option<oneshot::Sender<()>>,
}

impl Settled {
    #[must_use]
    pub fn new(
        verifier: Arc<dyn PaymentVerifier>,
        journal: Arc<dyn SettlementJournal>,
        record: SettlementRecord,
        owner_token: String,
        lease: Duration,
    ) -> Self {
        let scheme = record
            .scheme
            .expect("verified settlement record must declare its scheme");
        let stop_heartbeat = if record.phase == SettlementPhase::Held {
            Some(spawn_heartbeat(
                journal.clone(),
                record.nonce().to_string(),
                owner_token.clone(),
                lease,
            ))
        } else {
            None
        };
        Self {
            verifier,
            journal,
            nonce: record.nonce().to_string(),
            owner_token,
            hold: record.hold,
            scheme,
            quoted: record.authorization.amount_micros,
            lease,
            stop_heartbeat,
        }
    }

    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    #[must_use]
    pub fn has_hold(&self) -> bool {
        self.hold.is_some()
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    fn lease_until(&self) -> i64 {
        now_ms().saturating_add(self.lease.as_millis() as i64)
    }

    fn stop_heartbeat(&mut self) {
        if let Some(stop) = self.stop_heartbeat.take() {
            let _ = stop.send(());
        }
    }

    /// Persist capture intent, then replay the same Mint capture until its
    /// receipt is durable. A capture error never triggers release.
    pub async fn capture(
        mut self,
        actual: AmountMicros,
    ) -> Result<Option<Receipt>, TurnstileError> {
        self.stop_heartbeat();
        if actual > self.quoted {
            return Err(TurnstileError::Settlement(format!(
                "actual charge {} exceeds quoted maximum {}",
                actual.get(),
                self.quoted.get()
            )));
        }
        if actual == AmountMicros::ZERO {
            self.release().await?;
            return Ok(None);
        }
        let Some(hold) = self.hold.take() else {
            return Ok(None);
        };
        self.journal
            .prepare_capture(&self.nonce, &self.owner_token, actual, self.lease_until())
            .await?;
        let receipt = match self.verifier.capture(&hold, actual).await {
            Ok(receipt) => receipt,
            Err(error) => {
                let _ = self
                    .journal
                    .record_error(&self.nonce, &self.owner_token, &error.to_string())
                    .await;
                return Err(error);
            }
        };
        self.journal
            .complete_capture(&self.nonce, &self.owner_token, &receipt)
            .await?;
        Ok(Some(receipt))
    }

    /// Persist release intent before Mint I/O. A release error remains
    /// recoverable and can never be changed into capture.
    pub async fn release(mut self) -> Result<(), TurnstileError> {
        self.stop_heartbeat();
        let Some(hold) = self.hold.take() else {
            return Ok(());
        };
        self.journal
            .prepare_release(&self.nonce, &self.owner_token, self.lease_until())
            .await?;
        if let Err(error) = self.verifier.release(&hold).await {
            let _ = self
                .journal
                .record_error(&self.nonce, &self.owner_token, &error.to_string())
                .await;
            return Err(error);
        }
        self.journal
            .complete_release(&self.nonce, &self.owner_token)
            .await
    }
}

impl Drop for Settled {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn spawn_heartbeat(
    journal: Arc<dyn SettlementJournal>,
    nonce: String,
    owner_token: String,
    lease: Duration,
) -> oneshot::Sender<()> {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let interval_ms = (lease.as_millis() / 3).max(1) as u64;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = ticker.tick() => {
                    let lease_until = now_ms().saturating_add(lease.as_millis() as i64);
                    if let Err(error) = journal.renew(&nonce, &owner_token, lease_until).await {
                        eprintln!("turnstile settlement heartbeat stopped: {error}");
                        break;
                    }
                }
            }
        }
    });
    stop_tx
}

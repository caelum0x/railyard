//! Success-only hold settlement with RAII release-on-drop (SPEC ADR-0002).
//!
//! [`Settled`] is the handle Turnstile hands to the edge after a successful
//! verify. Exactly one terminal action runs:
//! - [`capture`](Settled::capture) settles the ACTUAL amount to the payee (the
//!   handler succeeded), or
//! - [`release`](Settled::release) frees the reservation (the handler failed,
//!   agent pays nothing).
//!
//! If the handle is dropped without either being called — a panic, an early
//! return, a bug — [`Drop`] fires a best-effort release so a reserved hold never
//! silently bills the agent. `aitp-transfer` verifies carry no hold, so their
//! [`Settled`] is inert (capture/release/drop are all no-ops).

use std::sync::Arc;

use aitp_client::{AmountMicros, HoldRef, Receipt, Scheme};

use crate::error::TurnstileError;
use crate::verifier::PaymentVerifier;

/// A settled-but-not-yet-captured hold. Holds the verifier so it can capture or
/// release, and takes the [`HoldRef`] on the first terminal action so it fires
/// once.
pub struct Settled {
    verifier: Arc<dyn PaymentVerifier>,
    hold: Option<HoldRef>,
    scheme: Scheme,
}

impl Settled {
    #[must_use]
    pub fn new(verifier: Arc<dyn PaymentVerifier>, hold: Option<HoldRef>, scheme: Scheme) -> Self {
        Self {
            verifier,
            hold,
            scheme,
        }
    }

    /// The scheme this settlement came from.
    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Whether a capturable hold is still outstanding.
    #[must_use]
    pub fn has_hold(&self) -> bool {
        self.hold.is_some()
    }

    /// Capture `actual` usage to the payee — the SUCCESS path. Consumes the
    /// handle so no drop-release follows. Returns the (lossy) [`Receipt`], or
    /// `None` for an `aitp-transfer` that had no hold to capture.
    pub async fn capture(
        mut self,
        actual: AmountMicros,
    ) -> Result<Option<Receipt>, TurnstileError> {
        match self.hold.take() {
            Some(hold) => Ok(Some(self.verifier.capture(&hold, actual).await?)),
            None => Ok(None),
        }
    }

    /// Release the hold without settling — the FAILURE path. Consumes the handle.
    pub async fn release(mut self) -> Result<(), TurnstileError> {
        if let Some(hold) = self.hold.take() {
            self.verifier.release(&hold).await?;
        }
        Ok(())
    }
}

impl Drop for Settled {
    fn drop(&mut self) {
        // If neither capture nor release ran, free the hold so a dropped handle
        // never bills the agent. Best-effort: only when a Tokio runtime is live
        // (it always is inside an axum handler); errors are swallowed.
        if let Some(hold) = self.hold.take() {
            let verifier = self.verifier.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = verifier.release(&hold).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::FakeVerifier;

    #[tokio::test]
    async fn capture_settles_actual_and_skips_drop_release() {
        let verifier = Arc::new(FakeVerifier::with_hold("h1"));
        let settled = Settled::new(verifier.clone(), Some(HoldRef("h1".into())), Scheme::Hold);

        let receipt = settled.capture(AmountMicros(900)).await.unwrap().unwrap();
        assert_eq!(receipt.amount_micros, AmountMicros(900));

        let calls = verifier.snapshot();
        assert_eq!(calls.captured, vec![("h1".to_string(), 900)]);
        assert!(
            calls.released.is_empty(),
            "captured hold must never release"
        );
    }

    #[tokio::test]
    async fn explicit_release_frees_hold_and_skips_capture() {
        let verifier = Arc::new(FakeVerifier::with_hold("h1"));
        let settled = Settled::new(verifier.clone(), Some(HoldRef("h1".into())), Scheme::Hold);

        settled.release().await.unwrap();

        let calls = verifier.snapshot();
        assert_eq!(calls.released, vec!["h1".to_string()]);
        assert!(calls.captured.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_uncaptured_hold_releases_via_raii() {
        let verifier = Arc::new(FakeVerifier::with_hold("h1"));
        {
            let settled = Settled::new(verifier.clone(), Some(HoldRef("h1".into())), Scheme::Hold);
            // Simulate a handler that neither captured nor released (panic/bug):
            // dropping the handle must free the reservation exactly once.
            drop(settled);
        }
        // Let the spawned best-effort release run to completion.
        for _ in 0..10 {
            if !verifier.snapshot().released.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            verifier.snapshot().released,
            vec!["h1".to_string()],
            "RAII drop releases the uncaptured hold exactly once"
        );
    }

    #[tokio::test]
    async fn transfer_settlement_is_inert() {
        // aitp-transfer verifies carry no hold: capture/release/drop are no-ops.
        let verifier = Arc::new(FakeVerifier::transfer());
        let settled = Settled::new(verifier.clone(), None, Scheme::Transfer);
        assert!(!settled.has_hold());

        let receipt = settled.capture(AmountMicros(100)).await.unwrap();
        assert!(receipt.is_none(), "no hold means no capture receipt");
        let calls = verifier.snapshot();
        assert!(calls.captured.is_empty());
        assert!(calls.released.is_empty());
    }
}

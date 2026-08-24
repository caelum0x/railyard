//! Injectable wall-clock. `SystemClock` for production; `FixedClock` for
//! deterministic tests. `now()` returns unix milliseconds.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of the current time in unix milliseconds. `Send + Sync` so it can
/// live behind a shared store.
pub trait Clock: Send + Sync {
    /// Current time, unix epoch milliseconds.
    fn now(&self) -> i64;
}

/// A shared clock is itself a clock: lets one injected `Arc<FixedClock>` (or
/// `Arc<dyn Clock>`) be handed to several collaborators (store, nonce store, and
/// the protocol state machine) so advancing it advances time everywhere — the
/// key to deterministic multi-component tests.
impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> i64 {
        (**self).now()
    }
}

/// Real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Deterministic clock for tests. Interior mutability so `now(&self)` works and
/// the clock stays `Send + Sync` behind a shared store.
#[derive(Debug)]
pub struct FixedClock {
    millis: AtomicI64,
}

impl FixedClock {
    /// Create a clock pinned to `millis`.
    pub fn new(millis: i64) -> Self {
        FixedClock {
            millis: AtomicI64::new(millis),
        }
    }

    /// Overwrite the current time.
    pub fn set(&self, millis: i64) {
        self.millis.store(millis, Ordering::SeqCst);
    }

    /// Advance the clock by `delta` milliseconds.
    pub fn advance(&self, delta: i64) {
        self.millis.fetch_add(delta, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.millis.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_is_deterministic() {
        let c = FixedClock::new(1_000);
        assert_eq!(c.now(), 1_000);
        c.advance(500);
        assert_eq!(c.now(), 1_500);
        c.set(42);
        assert_eq!(c.now(), 42);
    }

    #[test]
    fn system_clock_is_positive() {
        assert!(SystemClock.now() > 0);
    }
}

//! Wall-clock helper. AITP-402 expiries are unix milliseconds (SPEC §5); the
//! nonce store and challenge issuer stamp/compare against this single source.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds. Saturates to `0` if the system clock is
/// set before the epoch (never in practice) so callers need no error path.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

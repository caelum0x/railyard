//! Integer microdollar amounts.
//!
//! AITP-402 SPEC §1: `1 AIT = 1 USD = 1_000_000 µ$`, represented on the wire and
//! in storage as an integer number of microdollars. **Never floats.** Display
//! formatting (`$22.99`) is a UI concern only.

use serde::{Deserialize, Serialize};

/// Number of microdollars in one AIT (== one USD).
pub const MICROS_PER_AIT: i64 = 1_000_000;

/// An integer microdollar amount. Newtype so amounts never silently become
/// floats and never mix with unrelated integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AmountMicros(pub i64);

impl AmountMicros {
    pub const ZERO: AmountMicros = AmountMicros(0);

    #[must_use]
    pub const fn new(micros: i64) -> Self {
        Self(micros)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl From<i64> for AmountMicros {
    fn from(v: i64) -> Self {
        Self(v)
    }
}

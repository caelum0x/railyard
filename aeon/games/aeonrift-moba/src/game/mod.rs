//! Match lifecycle + the top-level match driver.
//!
//! - [`match_state`] — the [`Match`]: owns the world + MOBA state, runs the full tick,
//! - [`phases`]      — the Draft -> Loading -> InGame -> Ended lifecycle,
//! - [`economy`]     — per-team gold + XP.

pub mod economy;
pub mod match_state;
pub mod phases;

pub use economy::Economy;
pub use match_state::Match;
pub use phases::Phase;

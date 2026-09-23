//! Opaque, prefixed ids generated without external dependencies.
//!
//! A monotonic process counter is interleaved with the nanosecond clock so ids
//! stay unique even when the clock resolution collides under load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{seq:x}")
}

/// Fresh passport id, e.g. `psp_18f2c...`.
pub fn new_passport_id() -> String {
    format!("psp_{}", unique_suffix())
}

/// Fresh generation-record id, e.g. `gen_18f2c...`.
pub fn new_generation_id() -> String {
    format!("gen_{}", unique_suffix())
}

/// Fresh trained-artifact id, e.g. `art_18f2c...`.
pub fn new_artifact_id() -> String {
    format!("art_{}", unique_suffix())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_prefixed() {
        let a = new_passport_id();
        let b = new_passport_id();
        assert_ne!(a, b);
        assert!(a.starts_with("psp_"));
        assert!(new_generation_id().starts_with("gen_"));
    }
}

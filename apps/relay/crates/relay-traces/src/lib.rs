//! Per-API-key trace reads for Relay (RY-RL3, the OSS9 trace view).
//!
//! Every inference the gateway serves carries the tag
//! `tensorzero::api_key_public_id` on `chat_inferences` / `json_inferences`
//! (GIN-indexed). This crate turns those rows into a trace list and a trace
//! detail, joined with `model_inferences` for model, tokens, latency and cost.
//!
//! Scoping is the caller's job: the gateway passes either the caller's own key
//! (`TraceScope::Key`) or, for operator keys only, `TraceScope::All`. A trace
//! that belongs to another key is indistinguishable from a missing one, so the
//! detail read returns `None` (the HTTP layer maps it to 404).
//!
//! The retention job ([`retention`]) follows owner decision OD-RY2: full
//! prompt and output bodies for 14 days, metadata for 90 days.

pub mod query;
pub mod retention;
pub mod store;

pub use query::{
    DEFAULT_LIMIT, DEFAULT_WINDOW_HOURS, MAX_LIMIT, MAX_WINDOW_HOURS, TraceCursor, TraceListParams,
    TraceQueryError, TraceScope,
};
pub use retention::{RetentionError, RetentionPolicy, RetentionReport};
pub use store::{
    API_KEY_TAG, DEFAULT_SCHEMA, ModelCall, TraceDetail, TracePage, TraceStore, TraceStoreError,
    TraceSummary,
};

/// Validate a Postgres schema name used in generated SQL. Only lowercase
/// ASCII letters, digits and underscores are allowed, and it must not start
/// with a digit. This keeps the schema out of any injection path; values are
/// always bound as parameters.
pub fn valid_schema_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first == '_' => {}
        _ => return false,
    }
    name.len() <= 63 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::valid_schema_name;

    #[test]
    fn schema_names_are_restricted() {
        assert!(valid_schema_name("tensorzero"));
        assert!(valid_schema_name("relay_traces_test_01"));
        assert!(!valid_schema_name(""));
        assert!(!valid_schema_name("1abc"));
        assert!(!valid_schema_name("public; DROP TABLE x"));
        assert!(!valid_schema_name("Tensorzero"));
        assert!(!valid_schema_name(&"a".repeat(64)));
    }
}

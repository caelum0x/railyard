//! Thin convenience wrapper for recording audit events via the outbox.
//!
//! Each call enqueues an `audit_append` job rather than writing to the audit
//! log synchronously. This keeps the write off the hot request path and gives
//! durable retry semantics (the worker persists the append even if the handler
//! had a transient error after this call).
//!
//! All existing call sites (`crate::audit::record(...)`) continue to work
//! without modification — the outbox wiring is transparent to callers.

use uuid::Uuid;

use rithub_core::{OutboxStore, KIND_AUDIT_APPEND};

use crate::state::AppState;

/// Enqueue an audit event (best-effort — a failed enqueue is logged but never
/// propagates back to the request that triggered it).
#[allow(clippy::too_many_arguments)]
pub async fn record(
    state: &AppState,
    repo_id: Option<Uuid>,
    actor_id: Option<Uuid>,
    actor_name: &str,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    let payload = serde_json::json!({
        "repo_id": repo_id,
        "actor_id": actor_id,
        "actor_name": actor_name,
        "action": action,
        "target": target,
        "detail": detail,
    });
    if let Err(error) = state.store.enqueue_job(KIND_AUDIT_APPEND, payload).await {
        tracing::warn!(%error, action, "failed to enqueue audit job");
    }
}

//! Thin convenience wrapper over the append-only audit store, so handlers can
//! record a security event in one line without assembling the row struct.

use uuid::Uuid;

use crate::state::AppState;

/// Append an audit event (best-effort — a logging failure never fails the
/// request that triggered it).
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
    use rithub_core::{AuditStore, NewAuditEvent};
    let _ = state
        .store
        .record_event(NewAuditEvent {
            repo_id,
            actor_id,
            actor_name,
            action,
            target,
            detail,
        })
        .await;
}

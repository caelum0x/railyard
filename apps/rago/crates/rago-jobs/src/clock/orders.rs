//! Quote / order-form / order clocks.
//!
//! * `Clock::ExpireOrderFormsJob` — approved quote versions and unsigned order
//!   forms past their expiry are expired; each expired form emits
//!   `order_form.expired`.
//! * `Clock::ExecuteScheduledOrdersJob` — orders whose signed form scheduled
//!   execution (`execute_at`) are executed when that time arrives (the quote is
//!   invoiced, `order.executed`).

use chrono::Utc;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;

use crate::orders::{execute_order, OrderFlowError};
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// Orders executed per clock run.
const EXECUTE_BATCH: i64 = 100;

/// Expire lapsed offers. Quote versions with a signed order form are kept
/// (the signature is a commitment). Returns the number of order forms expired.
pub async fn expire_order_forms(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let versions = db.expire_quote_versions(now).await?;
    if !versions.is_empty() {
        tracing::info!(count = versions.len(), "expired quote versions");
    }

    let forms = db.expire_order_forms(now).await?;
    for form in &forms {
        record_webhook(
            db,
            form.organization_id,
            WebhookKind::OrderFormExpired,
            json!({
                "lago_id": form.id,
                "number": form.number,
                "status": "expired",
                "lago_customer_id": form.customer_id,
                "lago_quote_id": form.quote_id,
                "expires_at": form.expires_at.map(|t| t.and_utc().to_rfc3339()),
            }),
        )
        .await?;
    }
    Ok(forms.len() as u64)
}

/// Execute the orders whose scheduled execution time has arrived. A refused
/// execution (e.g. invalid quote lines) is logged and retried next run; one
/// failing order never blocks the others. Returns the number executed.
pub async fn execute_scheduled_orders(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let mut executed: u64 = 0;
    for (order_id, org) in db.orders_due_for_execution(now, EXECUTE_BATCH).await? {
        match execute_order(db, org, order_id).await {
            Ok(_) => executed += 1,
            // Executed concurrently (manual execute or another tick).
            Err(OrderFlowError::Invalid("order_not_pending")) => {}
            Err(e) => {
                tracing::error!(order_id = %order_id, organization_id = %org, error = %e, "scheduled order execution failed");
            }
        }
    }
    Ok(executed)
}

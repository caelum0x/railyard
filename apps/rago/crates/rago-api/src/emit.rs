//! Trigger-point webhook emission for REST handlers, plus usage-event emission
//! for metered products (Menivor reel generation).
//!
//! ## Webhook emission
//!
//! Thin best-effort wrapper over [`rago_jobs::webhook_emit::record_webhook`] —
//! the same endpoint-gated "materialize one pending `Webhook` per configured
//! endpoint" path the clock jobs use. At each Lago mutation trigger
//! (`Customers::UpsertFromApiService`, `CreditNotes::CreateService`,
//! `Entitlement::Features::*Service`, …) we record the signed-ready envelope
//! built from the [`WebhookKind`] catalog for the delivery worker to pick up.
//!
//! Emission is best-effort: the handler's state change has already committed, so
//! a no-endpoint no-op or a persistence hiccup must never turn a successful
//! mutation into an error (matching Lago's fire-and-forget `perform_later`).
//!
//! ## Usage-event emission (reel generation)
//!
//! Menivor's generation gateway reports each completed reel render to rago; the
//! [`reel_usage_events`] helper expands one [`ReelRenderUsage`] into the billable
//! usage events it should emit (against the [`reel_metric`] catalog), and
//! [`ingest_event`] validates + persists each one through the exact same path as
//! the public `POST /events` ingest. `render_id` is the idempotency key, so a
//! render reported twice never double-bills.

use rago_billing::events::{build_event, validate_creation, EventParams, EventValidationError};
use rago_core::models::Event;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Record a pending outbound webhook for a REST trigger point. Errors and the
/// no-endpoint case are swallowed (the resource is already persisted).
pub(crate) async fn webhook(db: &Db, organization_id: Uuid, kind: WebhookKind, object: Value) {
    let _ = rago_jobs::webhook_emit::record_webhook(db, organization_id, kind, object).await;
}

// ---- reel usage events -----------------------------------------------------

/// Canonical `billable_metric.code` values for Menivor reel generation. rago
/// must be provisioned with one billable metric per code before reel usage can
/// be priced — see [`crate::usage::reel_billing_catalog`] for request-ready
/// definitions.
pub mod reel_metric {
    /// One event per billable reel produced (`count_agg`).
    pub const GENERATED: &str = "reel_generated";
    /// Output video duration billed per whole second (`sum_agg`).
    pub const RENDER_SECONDS: &str = "reel_render_seconds";
    /// AI model credits (LLM / image / voice) consumed by the render
    /// (`sum_agg`).
    pub const AI_CREDITS: &str = "reel_ai_credits";
    /// Event `properties` key carrying the [`RENDER_SECONDS`] quantity.
    pub const RENDER_SECONDS_FIELD: &str = "seconds";
    /// Event `properties` key carrying the [`AI_CREDITS`] quantity.
    pub const AI_CREDITS_FIELD: &str = "credits";
}

/// `plan.code` for the reel-generation plan the reel metrics are charged on.
pub const REEL_PLAN_CODE: &str = "menivor_reels";

/// The billable facts of one completed reel render, as Menivor's generation
/// gateway reports them.
///
/// `render_id` is the idempotency key: every derived usage event's
/// `transaction_id` is `{render_id}:{metric_code}`, so re-reporting the same
/// render is a no-op (rago's unique `(subscription, transaction_id)` index
/// rejects the duplicate and it is surfaced as "already billed", never a
/// second charge).
#[derive(Debug, Clone, Deserialize)]
pub struct ReelRenderUsage {
    /// Stable id of the render (the idempotency key).
    pub render_id: String,
    /// The subscription being metered.
    pub external_subscription_id: String,
    /// Whether this render produced a billable reel (emits
    /// [`reel_metric::GENERATED`]). Defaults to `true`.
    #[serde(default = "default_true")]
    pub generated: bool,
    /// Output duration in whole seconds (emits [`reel_metric::RENDER_SECONDS`]
    /// when present and `> 0`).
    #[serde(default)]
    pub render_seconds: Option<i64>,
    /// AI model credits consumed (emits [`reel_metric::AI_CREDITS`] when present
    /// and `> 0`).
    #[serde(default)]
    pub ai_credits: Option<i64>,
    /// Event timestamp (unix seconds); defaults to the ingest time.
    #[serde(default)]
    pub timestamp: Option<f64>,
}

fn default_true() -> bool {
    true
}

/// Build one usage event's params for a reel metric dimension. `field` carries
/// the `(properties_key, quantity)` for `sum_agg` metrics; `None` for the
/// `count_agg` metric (which needs no numeric property).
fn reel_event_params(u: &ReelRenderUsage, code: &str, field: Option<(&str, i64)>) -> EventParams {
    let properties = match field {
        Some((key, value)) => json!({ key: value }),
        None => json!({}),
    };
    EventParams {
        code: code.to_string(),
        transaction_id: format!("{}:{}", u.render_id, code),
        external_subscription_id: u.external_subscription_id.clone(),
        properties,
        timestamp: u.timestamp,
        precise_total_amount_cents: None,
    }
}

/// Expand a reel render into the billable usage events it should emit: always
/// [`reel_metric::GENERATED`] when `generated`, plus per-quantity
/// [`reel_metric::RENDER_SECONDS`] / [`reel_metric::AI_CREDITS`] when those are
/// present and positive. Deterministic and idempotent by `render_id`.
pub fn reel_usage_events(u: &ReelRenderUsage) -> Vec<EventParams> {
    let mut out = Vec::new();
    if u.generated {
        out.push(reel_event_params(u, reel_metric::GENERATED, None));
    }
    if let Some(seconds) = u.render_seconds.filter(|s| *s > 0) {
        out.push(reel_event_params(
            u,
            reel_metric::RENDER_SECONDS,
            Some((reel_metric::RENDER_SECONDS_FIELD, seconds)),
        ));
    }
    if let Some(credits) = u.ai_credits.filter(|c| *c > 0) {
        out.push(reel_event_params(
            u,
            reel_metric::AI_CREDITS,
            Some((reel_metric::AI_CREDITS_FIELD, credits)),
        ));
    }
    out
}

/// Outcome of ingesting one usage event.
pub(crate) enum Ingest {
    /// The event was validated and persisted.
    Created(Box<Event>),
    /// An event with this `transaction_id` already exists for the subscription;
    /// ingest is a no-op (idempotent replay), not an error.
    Duplicate,
}

/// Validate + persist a single usage event through the same path as the public
/// `POST /events` ingest: resolve the subscription + metric, dedup by
/// `transaction_id`, and insert. A pre-existing `transaction_id` yields
/// [`Ingest::Duplicate`] rather than an error so callers can replay safely;
/// genuine validation failures (unknown subscription/metric, non-numeric
/// properties, missing transaction id) are returned as [`ApiError`].
pub(crate) async fn ingest_event(
    st: &AppState,
    org: Uuid,
    params: EventParams,
) -> ApiResult<Ingest> {
    let sub = st
        .db
        .find_subscription_by_external_id(org, &params.external_subscription_id)
        .await?;
    let sub_ext_ids: Vec<String> = sub
        .as_ref()
        .map(|s| vec![s.external_id.clone()])
        .unwrap_or_default();
    let metric = st.db.find_metric_by_code(org, &params.code).await?;
    let exists = st
        .db
        .event_exists(org, &params.external_subscription_id, &params.transaction_id)
        .await?;

    // Validate everything except the "already exists" case (passed as `false`)
    // so we can distinguish a real conflict — which is an idempotent no-op — from
    // the hard validation errors that must surface.
    if let Err(e) = validate_creation(&params, &sub_ext_ids, metric.as_ref(), false) {
        // `event.error` — the integrator learns which event was rejected and why.
        webhook(
            &st.db,
            org,
            WebhookKind::EventError,
            json!({
                "transaction_id": params.transaction_id,
                "lago_customer_id": sub.as_ref().map(|s| s.customer_id),
                "external_subscription_id": params.external_subscription_id,
                "code": params.code,
                "error": format!("{e:?}"),
                "status": 422,
            }),
        )
        .await;
        return Err(map_event_error(e));
    }
    if exists {
        return Ok(Ingest::Duplicate);
    }

    let mut event = build_event(org, &params, crate::handlers::now());
    event.customer_id = sub.as_ref().map(|s| s.customer_id);
    event.subscription_id = sub.as_ref().map(|s| s.id);

    // Real-time prepaid wallet: price the event now and reserve it on the
    // customer's Mint account before accepting it.
    let reserved = match sub.as_ref() {
        Some(sub) => realtime_reserve(st, org, sub, &event).await?,
        None => false,
    };

    // The DB unique index is the authoritative dedup guard: if a concurrent
    // request won the race between the check above and this insert, treat it as
    // the same idempotent no-op.
    if !st.db.insert_event(&event).await? {
        if reserved {
            if let Some(sub) = sub.as_ref() {
                let transport = rago_integrations::default_transport();
                if let Err(e) = rago_jobs::wallet_realtime::release_event_hold(&st.db, &transport, sub.customer_id, &event.transaction_id).await {
                    tracing::warn!(error = %e, "could not release the hold of a duplicate event");
                }
            }
        }
        return Ok(Ingest::Duplicate);
    }
    // Pay-in-advance charges on this metric bill the event right away.
    if let Some(sub) = sub.as_ref() {
        if let Err(e) = rago_jobs::pay_in_advance::enqueue_for_event(&st.db, sub, &event).await {
            tracing::error!(
                event_id = %event.id,
                error = %e,
                "could not enqueue pay-in-advance billing for event"
            );
        }
    }
    Ok(Ingest::Created(Box::new(event)))
}

/// Reserve `event` on the customer's real-time wallet. `Ok(true)` when a Mint
/// hold was placed; an error rejects the event (insufficient balance under the
/// `reject` policy, or Mint unavailable).
async fn realtime_reserve(
    st: &AppState,
    org: Uuid,
    sub: &rago_core::models::Subscription,
    event: &rago_core::models::Event,
) -> ApiResult<bool> {
    use rago_jobs::wallet_realtime::{reserve_for_event, Reservation};
    let transport = rago_integrations::default_transport();
    let outcome = match reserve_for_event(&st.db, &transport, org, sub, event).await {
        Ok(o) => o,
        Err(rago_jobs::JobError::Db(e)) => return Err(ApiError::from(e)),
        Err(e) => {
            tracing::error!(error = %e, "real-time wallet reservation failed");
            return Err(ApiError::Unavailable("wallet_realtime_unavailable".into()));
        }
    };
    let rejection = |error: &str, amount_cents: i64, status: u16| {
        json!({
            "transaction_id": event.transaction_id,
            "lago_customer_id": sub.customer_id,
            "external_subscription_id": sub.external_id,
            "code": event.code,
            "error": error,
            "amount_cents": amount_cents,
            "status": status,
        })
    };
    match outcome {
        Reservation::Reserved { .. } => Ok(true),
        Reservation::NotApplicable | Reservation::Free => Ok(false),
        Reservation::Flagged { cents, reason } => {
            let error = if reason == "insufficient_funds" {
                "wallet_balance_insufficient".to_string()
            } else {
                format!("wallet_realtime_unavailable: {reason}")
            };
            webhook(&st.db, org, WebhookKind::EventError, rejection(&error, cents, 200)).await;
            Ok(false)
        }
        Reservation::Rejected { cents } => {
            webhook(&st.db, org, WebhookKind::EventError, rejection("wallet_balance_insufficient", cents, 422)).await;
            Err(ApiError::validation("wallet", "wallet_balance_insufficient"))
        }
        Reservation::Unavailable { reason } => {
            tracing::warn!(reason = %reason, "event refused: real-time wallet cannot reserve");
            Err(ApiError::Unavailable("wallet_realtime_unavailable".into()))
        }
    }
}

/// Map a billing-layer event validation error to the REST-facing [`ApiError`],
/// matching the codes the public `POST /events` handler returns.
pub(crate) fn map_event_error(e: EventValidationError) -> ApiError {
    match e {
        EventValidationError::SubscriptionNotFound => ApiError::NotFound("subscription".into()),
        EventValidationError::BillableMetricNotFound => {
            ApiError::NotFound("billable_metric".into())
        }
        EventValidationError::TransactionId => {
            ApiError::validation("transaction_id", "value_is_missing_or_already_exists")
        }
        EventValidationError::InvalidProperties => {
            ApiError::validation("properties", "value_is_not_valid_number")
        }
        EventValidationError::InvalidTimestamp => {
            ApiError::validation("timestamp", "invalid_format")
        }
    }
}

//! Subscription-lifecycle clocks.
//!
//! Ports `Clock::{ActivateSubscriptions, TerminateEndedSubscriptions,
//! ExpireIncompleteSubscriptions, FreeTrialSubscriptionsBiller,
//! SubscriptionsToBeTerminated}Job`. Each is an idempotent SQL sweep run per
//! tick; money-affecting selections carry their own idempotency gate.

use chrono::{Duration, Utc};
use rago_db::lifecycle::SubscriptionRef;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::scheduler::{bill_subscription_external, today};
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// `Clock::ActivateSubscriptionsJob` — activate pending subs whose start date
/// has arrived. Mirrors `Subscriptions::ActivateAllPendingService` → per-sub
/// `ActivateService`, which emits `subscription.started` on activation.
///
/// A subscription charged upfront (pay-in-advance plan with a fee, no trial)
/// whose customer has no usable payment method at the organization's provider
/// is moved to `incomplete` instead (`subscription.incomplete`); incomplete
/// subscriptions are re-checked every tick and activated as soon as a payment
/// method exists, until the ExpireIncompleteSubscriptions clock cancels them.
///
/// Each transition is a guarded single-row UPDATE (`WHERE status = ...`), so a
/// re-run or a concurrent tick never emits a duplicate webhook.
pub async fn activate_subscriptions(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let mut activated: u64 = apply_scheduled_downgrades(db).await?;

    for sub in db.pending_subscriptions_due(now).await? {
        if activation_blocked_on_payment_method(db, &sub).await? {
            if db.mark_subscription_incomplete(sub.id).await? {
                record_webhook(
                    db,
                    sub.organization_id,
                    WebhookKind::SubscriptionIncomplete,
                    subscription_object(&sub, "incomplete"),
                )
                .await?;
            }
            continue;
        }
        if db.activate_pending_subscription(sub.id).await? {
            activated += 1;
            record_webhook(
                db,
                sub.organization_id,
                WebhookKind::SubscriptionStarted,
                subscription_object(&sub, "active"),
            )
            .await?;
        }
    }

    for sub in db.incomplete_subscriptions().await? {
        if activation_blocked_on_payment_method(db, &sub).await? {
            continue;
        }
        if db.activate_incomplete_subscription(sub.id).await? {
            activated += 1;
            record_webhook(
                db,
                sub.organization_id,
                WebhookKind::SubscriptionStarted,
                subscription_object(&sub, "active"),
            )
            .await?;
        }
    }
    Ok(activated)
}

/// Scheduled downgrades whose boundary arrived (`Subscriptions::PlanUpgradeService`
/// for the queued next subscription): bill the outgoing plan's closing period,
/// then atomically terminate it and activate the downgraded subscription.
///
/// * The closing period of a pay-in-arrears outgoing plan is billed here (the
///   billing run only bills active subscriptions, and the outgoing one is
///   terminated right after). The bill is guarded per (subscription, period),
///   so a billing run that already billed it is not repeated. A billing error
///   leaves both subscriptions untouched and the swap is retried next tick.
/// * A pay-in-advance outgoing plan already collected its fee for the closing
///   period at the start of it, but not that period's usage: its usage
///   charges are billed on a closing-usage invoice (no subscription fee),
///   guarded per (subscription, period). The regular billing run skips such a
///   subscription at the boundary, so it is never billed the next period's fee.
/// * An outgoing subscription that is no longer active (terminated by the
///   operator meanwhile) cancels the scheduled downgrade.
pub async fn apply_scheduled_downgrades(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let transport = rago_integrations::default_transport();
    let mut applied = 0;
    for (outgoing, incoming) in db.due_downgrades(now).await? {
        if !matches!(outgoing.status, rago_core::enums::SubscriptionStatus::Active) {
            for canceled in db.cancel_scheduled_downgrades(outgoing.id).await? {
                record_webhook(
                    db,
                    canceled.organization_id,
                    WebhookKind::SubscriptionCanceled,
                    json!({ "lago_id": canceled.id, "external_id": canceled.external_id, "status": "canceled" }),
                )
                .await?;
            }
            continue;
        }
        let boundary = incoming.subscription_at.unwrap_or_else(Utc::now);
        let plan = db.find_plan(outgoing.plan_id).await?;
        let closing = if plan.pay_in_advance {
            crate::scheduler::bill_closing_charges_and_collect(
                db,
                &transport,
                &outgoing,
                &plan,
                boundary.date_naive(),
                true,
            )
            .await
            .map(|_| ())
        } else {
            let interval = crate::scheduler::interval_of(&plan.interval);
            let anniversary = matches!(outgoing.billing_time, rago_core::enums::BillingTime::Anniversary);
            crate::scheduler::bill_and_collect(
                db,
                &transport,
                &outgoing,
                &plan,
                interval,
                anniversary,
                boundary.date_naive(),
                true,
            )
            .await
            .map(|_| ())
        };
        if let Err(e) = closing {
            tracing::error!(
                subscription_id = %outgoing.id,
                error = %e,
                "closing invoice before a scheduled downgrade failed; retrying next tick"
            );
            continue;
        }
        if !db.swap_downgrade(outgoing.id, incoming.id, boundary.naive_utc()).await? {
            continue;
        }
        applied += 1;
        record_webhook(
            db,
            outgoing.organization_id,
            WebhookKind::SubscriptionTerminated,
            json!({ "lago_id": outgoing.id, "external_id": outgoing.external_id, "status": "terminated", "next_subscription_id": incoming.id }),
        )
        .await?;
        record_webhook(
            db,
            incoming.organization_id,
            WebhookKind::SubscriptionStarted,
            json!({ "lago_id": incoming.id, "external_id": incoming.external_id, "status": "active", "previous_subscription_id": outgoing.id }),
        )
        .await?;
    }
    Ok(applied)
}

fn subscription_object(sub: &SubscriptionRef, status: &str) -> serde_json::Value {
    json!({
        "lago_id": sub.id,
        "external_id": sub.external_id,
        "lago_customer_id": sub.customer_id,
        "status": status,
    })
}

/// Whether activating `sub` must wait for a payment method: its plan charges
/// upfront and the customer has none at the organization's payment provider.
/// Without a configured provider the invoice is collected manually, so nothing
/// blocks activation; a provider lookup that errors (outage, non-live build)
/// never blocks either — only a definite "no payment method" does.
pub async fn activation_blocked_on_payment_method(db: &Db, sub: &SubscriptionRef) -> Result<bool, JobError> {
    if !db.subscription_requires_payment_method(sub.id).await? {
        return Ok(false);
    }
    payment_method_missing(db, sub.organization_id, sub.customer_id).await
}

/// Whether the customer definitely has no payment method at the
/// organization's provider (see [`activation_blocked_on_payment_method`]).
pub async fn payment_method_missing(db: &Db, org: Uuid, customer_id: Uuid) -> Result<bool, JobError> {
    let Some(provider) = crate::collection::resolve_provider(db, org).await? else {
        return Ok(false);
    };
    let stored = db.provider_customer(customer_id, &provider.kind).await?;
    let has_stored_method = stored
        .as_ref()
        .and_then(|(_, pm)| pm.as_deref())
        .is_some_and(|pm| !pm.trim().is_empty());
    if has_stored_method {
        return Ok(false);
    }
    match provider.kind.as_str() {
        "stripe" => {
            // The method may exist at Stripe without being stored yet.
            let Some(customer) = db.find_customer(customer_id).await? else {
                return Ok(false);
            };
            let transport = rago_integrations::default_transport();
            match crate::collection::ensure_stripe_customer(db, &transport, &provider.secret, &customer).await {
                Ok((_, pm)) => Ok(pm.filter(|p| !p.is_empty()).is_none()),
                Err(e) => {
                    tracing::warn!(
                        customer_id = %customer_id,
                        error = %e,
                        "payment method lookup failed; not blocking activation"
                    );
                    Ok(false)
                }
            }
        }
        // GoCardless charges a mandate, Adyen a stored shopper reference: with
        // neither stored there is nothing to charge.
        "gocardless" => Ok(true),
        "adyen" => Ok(stored.and_then(|(pcid, _)| pcid).filter(|p| !p.is_empty()).is_none()),
        _ => Ok(false),
    }
}

/// `Clock::TerminateEndedSubscriptionsJob` → per-sub `Subscriptions::TerminateService`,
/// which emits `subscription.terminated` after commit.
///
/// The to-be-terminated ids are captured before the update; a re-run selects
/// nothing (status is no longer active), so the webhook fires exactly once per
/// subscription.
pub async fn terminate_ended_subscriptions(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let ending = sqlx::query(
        "SELECT id, organization_id, external_id FROM subscriptions \
         WHERE status = 1 AND ending_at IS NOT NULL AND ending_at <= $1",
    )
    .bind(now)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let transport = rago_integrations::default_transport();
    let mut terminated = 0;
    for row in &ending {
        let sub_id: Uuid = row.get("id");
        let org: Uuid = row.get("organization_id");
        let external_id: String = row.get("external_id");
        // A subscription ending exactly on a period boundary (cancel at period
        // end) is billed its final period before it stops being active: the
        // billing run only bills active subscriptions. A billing error leaves
        // it active so the next tick retries.
        if let Err(e) = bill_final_period(db, &transport, sub_id).await {
            tracing::error!(
                subscription_id = %sub_id,
                error = %e,
                "final invoice before termination failed; retrying next tick"
            );
            continue;
        }
        let done = sqlx::query(
            "UPDATE subscriptions SET status = 2, terminated_at = now(), updated_at = now() \
             WHERE id = $1 AND status = 1 AND ending_at IS NOT NULL AND ending_at <= $2",
        )
        .bind(sub_id)
        .bind(now)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?
        .rows_affected();
        if done == 0 {
            continue; // a concurrent tick won
        }
        terminated += 1;
        record_webhook(
            db,
            org,
            WebhookKind::SubscriptionTerminated,
            json!({ "lago_id": sub_id, "external_id": external_id }),
        )
        .await?;
    }
    Ok(terminated)
}

/// Bill the final invoice of a subscription whose `ending_at` has arrived.
///
/// * Ending on a billing boundary (00:00 UTC of a rollover day, cancel at
///   period end): the closing period in full for a pay-in-arrears plan, the
///   closing period's usage for a pay-in-advance plan (its fee was collected
///   upfront).
/// * Ending mid-period: the prorated final invoice (fee for the days lived in
///   arrears; usage through the termination; the unused days of a
///   pay-in-advance fee credited back) — see [`crate::termination`].
///
/// Every invoice is guarded per (subscription, period) and the credit note by
/// the fee's credited amount, so a billing run that already billed the period,
/// or a retried tick, is not repeated.
async fn bill_final_period<T: rago_integrations::HttpTransport>(
    db: &Db,
    transport: &T,
    subscription_id: Uuid,
) -> Result<(), JobError> {
    let sub = db.find_subscription(subscription_id).await?;
    let Some(ending_at) = sub.ending_at else { return Ok(()) };
    let plan = db.find_plan(sub.plan_id).await?;
    let end = ending_at.naive_utc();
    let end_date = end.date();
    let interval = crate::scheduler::interval_of(&plan.interval);
    let anniversary = matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary);
    let sub_at = sub.subscription_at.map(|t| t.date_naive()).unwrap_or(end_date);
    let at_boundary = end.time() == chrono::NaiveTime::MIN
        && rago_billing::dates::due_for_billing(interval, anniversary, sub_at, end_date);
    if !at_boundary {
        crate::termination::bill_mid_period_termination(db, transport, &sub, &plan, end, true).await?;
        return Ok(());
    }
    if sub.started_at.is_some_and(|t| t.date_naive() >= end_date) {
        return Ok(());
    }
    if plan.pay_in_advance {
        crate::scheduler::bill_closing_charges_and_collect(db, transport, &sub, &plan, end_date, true).await?;
    } else {
        crate::scheduler::bill_and_collect(db, transport, &sub, &plan, interval, anniversary, end_date, true).await?;
    }
    Ok(())
}

/// `Subscriptions::CreateService` → `BillSubscriptionJob(invoicing_reason:
/// subscription_starting)`: a pay-in-advance subscription without a free
/// trial is billed its first period when it starts — in full for anniversary
/// billing (the period starts that day), prorated from the start day to the
/// period end for calendar billing started mid-period (which the daily run,
/// billing only on rollover days, would otherwise never bill). A trial
/// subscription is billed when its trial ends ([`free_trial_biller`]).
///
/// Runs every tick for subscriptions still in their first period whose
/// period has no invoice yet; the per-period re-bill guard makes it
/// idempotent with the daily run (both key the invoice by the same period).
pub async fn bill_starting_subscriptions(db: &Db) -> Result<u64, JobError> {
    let transport = rago_integrations::default_transport();
    let today = today();
    let mut billed = 0;
    for sub in db.starting_pay_in_advance_subscriptions(Utc::now().naive_utc()).await? {
        let plan = db.find_plan(sub.plan_id).await?;
        let span = crate::scheduler::span_of(&sub, &plan, today);
        if !span.in_first_period(today) {
            continue;
        }
        let period = span.period_of(today);
        let already = db
            .existing_period_invoice(
                sub.id,
                period.from.and_hms_opt(0, 0, 0).unwrap_or_default(),
                period.to.and_hms_opt(23, 59, 59).unwrap_or_default(),
            )
            .await?
            .is_some();
        if already || crate::scheduler::leaves_at_boundary(db, &sub, &plan, today).await? {
            continue;
        }
        match crate::scheduler::bill_and_collect(
            db,
            &transport,
            &sub,
            &plan,
            span.interval,
            span.anniversary,
            today,
            true,
        )
        .await
        {
            Ok(outcome) if outcome.created => billed += 1,
            Ok(_) => {}
            Err(e) => tracing::error!(
                subscription_id = %sub.id,
                error = %e,
                "first-period invoice failed; retrying next tick"
            ),
        }
    }
    Ok(billed)
}

/// `Clock::ExpireIncompleteSubscriptionsJob` — incomplete subs whose
/// activation window (Lago default 3 days from their start date) elapsed
/// without a payment method → canceled, each emitting `subscription.canceled`.
pub async fn expire_incomplete_subscriptions(db: &Db) -> Result<u64, JobError> {
    let cutoff = (Utc::now() - Duration::days(INCOMPLETE_ACTIVATION_WINDOW_DAYS)).naive_utc();
    let canceled = db.cancel_expired_incomplete_subscriptions(cutoff).await?;
    for sub in &canceled {
        record_webhook(
            db,
            sub.organization_id,
            WebhookKind::SubscriptionCanceled,
            subscription_object(sub, "canceled"),
        )
        .await?;
    }
    Ok(canceled.len() as u64)
}

/// How long an incomplete subscription waits for a payment method.
pub const INCOMPLETE_ACTIVATION_WINDOW_DAYS: i64 = 3;

/// `Clock::FreeTrialSubscriptionsBillerJob` → `Subscriptions::FreeTrialBillingService`.
///
/// Selects active subscriptions whose plan trial period has now elapsed and
/// that have not yet had their trial closed (`trial_ended_at IS NULL`). For
/// each: stamp `trial_ended_at`, bill the first period when the plan is
/// pay-in-advance (guarded/idempotent), and record the `subscription.trial_ended`
/// webhook. Returns the number of trials closed.
///
/// Idempotency: `trial_ended_at` is the gate — once stamped, the subscription is
/// no longer selected, so a re-run neither re-bills nor re-alerts. The billing
/// path itself is additionally guarded by the (subscription, period) unique
/// index.
pub async fn free_trial_biller(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let rows = sqlx::query(
        "SELECT s.id, s.organization_id, s.external_id, \
                (s.started_at + (p.trial_period * INTERVAL '1 day')) AS trial_end, \
                p.pay_in_advance \
         FROM subscriptions s JOIN plans p ON p.id = s.plan_id \
         WHERE s.status = 1 AND p.trial_period > 0 AND s.trial_ended_at IS NULL \
           AND s.started_at IS NOT NULL \
           AND s.started_at + (p.trial_period * INTERVAL '1 day') <= $1",
    )
    .bind(now)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let mut ended: u64 = 0;
    for row in &rows {
        let sub_id: Uuid = row.get("id");
        let org: Uuid = row.get("organization_id");
        let external_id: String = row.get("external_id");
        let trial_end: chrono::NaiveDateTime = row.get("trial_end");
        let pay_in_advance: bool = row.get("pay_in_advance");

        // Close the trial first — this is the idempotency gate.
        sqlx::query(
            "UPDATE subscriptions SET trial_ended_at = $2, updated_at = now() \
             WHERE id = $1 AND trial_ended_at IS NULL",
        )
        .bind(sub_id)
        .bind(trial_end)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;

        // Bill the first (subscription-fee) period for pay-in-advance plans. A
        // fresh trial subscription has no usage events, so this yields the
        // subscription fee only, and the re-bill guard makes it idempotent.
        if pay_in_advance {
            bill_subscription_external(db, org, &external_id, today()).await?;
        }

        // Record the trial-ended webhook (mirrors SendWebhookJob enqueue).
        record_webhook(
            db,
            org,
            WebhookKind::SubscriptionTrialEnded,
            json!({ "lago_id": sub_id, "external_id": external_id }),
        )
        .await?;

        ended += 1;
    }
    Ok(ended)
}

/// `Clock::SubscriptionsToBeTerminatedJob` — send a `subscription.termination_alert`
/// webhook a fixed number of days before an active subscription's `ending_at`
/// (Lago default: 15 and 45 days). De-duplicated per subscription per day.
/// Returns the number of alerts emitted.
pub async fn subscriptions_to_be_terminated(db: &Db) -> Result<u64, JobError> {
    let today = today();
    // Lago default LAGO_SUBSCRIPTION_TERMINATION_ALERT_SENT_AT_DAYS = "15,45".
    let alert_days = [15i64, 45i64];
    let mut emitted: u64 = 0;

    for days in alert_days {
        let target = today + Duration::days(days);
        let rows = sqlx::query(
            "SELECT s.id, s.organization_id, s.external_id \
             FROM subscriptions s \
             WHERE s.status = 1 AND s.ending_at IS NOT NULL \
               AND DATE(s.ending_at) = $1",
        )
        .bind(target)
        .fetch_all(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;

        for row in &rows {
            let sub_id: Uuid = row.get("id");
            let org: Uuid = row.get("organization_id");
            let external_id: String = row.get("external_id");

            // Skip if we already alerted for this subscription today. The
            // enveloped payload nests the object under its `object_type`, so the
            // subscription id lives at `payload -> 'subscription' ->> 'lago_id'`.
            let already: Option<i64> = sqlx::query_scalar(
                "SELECT 1::bigint FROM webhooks \
                 WHERE webhook_type = 'subscription.termination_alert' \
                   AND payload->'subscription'->>'lago_id' = $1 AND created_at::date = $2 LIMIT 1",
            )
            .bind(sub_id.to_string())
            .bind(today)
            .fetch_optional(&db.pool)
            .await
            .map_err(rago_db::DbError::from)?;
            if already.is_some() {
                continue;
            }

            if record_webhook(
                db,
                org,
                WebhookKind::SubscriptionTerminationAlert,
                json!({ "lago_id": sub_id, "external_id": external_id, "days_before": days }),
            )
            .await?
            {
                emitted += 1;
            }
        }
    }
    Ok(emitted)
}

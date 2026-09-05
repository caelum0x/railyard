//! Subscription-lifecycle clocks.
//!
//! Ports `Clock::{ActivateSubscriptions, TerminateEndedSubscriptions,
//! ExpireIncompleteSubscriptions, FreeTrialSubscriptionsBiller,
//! SubscriptionsToBeTerminated}Job`. Each is an idempotent SQL sweep run per
//! tick; money-affecting selections carry their own idempotency gate.

use chrono::{Duration, Utc};
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
/// The ids that will transition are captured *before* the update so the webhook
/// carries each activated subscription; a re-run selects nothing (they are no
/// longer pending), so no duplicate `subscription.started` is emitted.
pub async fn activate_subscriptions(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let due = sqlx::query(
        "SELECT id, organization_id, external_id FROM subscriptions \
         WHERE status = 0 AND subscription_at IS NOT NULL AND subscription_at <= $1",
    )
    .bind(now)
    .fetch_all(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;

    let activated = db.activate_due_subscriptions(now).await?;

    for row in &due {
        let sub_id: Uuid = row.get("id");
        let org: Uuid = row.get("organization_id");
        let external_id: String = row.get("external_id");
        record_webhook(
            db,
            org,
            WebhookKind::SubscriptionStarted,
            json!({ "lago_id": sub_id, "external_id": external_id }),
        )
        .await?;
    }
    Ok(activated)
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

    let terminated = db.terminate_ended_subscriptions(now).await?;

    for row in &ending {
        let sub_id: Uuid = row.get("id");
        let org: Uuid = row.get("organization_id");
        let external_id: String = row.get("external_id");
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

/// `Clock::ExpireIncompleteSubscriptionsJob` — incomplete subs older than the
/// activation window (Lago default 3 days) → canceled.
pub async fn expire_incomplete_subscriptions(db: &Db) -> Result<u64, JobError> {
    let cutoff = (Utc::now() - Duration::days(3)).naive_utc();
    Ok(db.expire_incomplete_subscriptions(cutoff).await?)
}

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

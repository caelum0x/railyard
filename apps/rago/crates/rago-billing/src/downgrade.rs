//! Scheduled (pending) plan downgrade.
//!
//! Faithful port of the shape of `Subscriptions::PlanDowngradeService`. Unlike an
//! **upgrade** (which takes effect immediately, terminating the old plan
//! mid-period and issuing an automatic credit note for the unused days — see
//! [`crate::subscriptions::upgrade_subscription`]), a **downgrade** is deferred:
//!
//! > "When downgrading a subscription, we keep the current one active until the
//! > next billing day. The new subscription will become active at this date."
//!
//! So scheduling a downgrade creates a **pending** next-subscription on the new
//! (cheaper) plan; the current subscription keeps running unchanged for the rest
//! of the period, and **no mid-period credit is issued**. At the next billing
//! boundary the current subscription is terminated and the pending one activated.

use chrono::{DateTime, Utc};
use rago_core::enums::SubscriptionStatus;
use rago_core::models::{Plan, Subscription};
use uuid::Uuid;

/// The outcome of scheduling a downgrade.
pub struct ScheduledDowngrade {
    /// The current subscription — unchanged and still active for the rest of
    /// the period (no termination, no credit).
    pub current_subscription: Subscription,
    /// The new, **pending** subscription on the downgraded plan; it activates at
    /// the next billing date.
    pub pending_subscription: Subscription,
}

/// `Subscriptions::PlanDowngradeService#call` (the standard "started, not
/// starting-in-the-future" path): create a pending next-subscription on
/// `new_plan`. The current subscription is returned untouched — crucially, **no
/// credit note / proration is produced**, which is the whole point of a
/// downgrade versus an upgrade.
pub fn schedule_downgrade(
    current: &Subscription,
    new_plan: &Plan,
    now: DateTime<Utc>,
) -> ScheduledDowngrade {
    let mut pending = current.clone();
    pending.id = Uuid::new_v4();
    pending.plan_id = new_plan.id;
    // The new subscription belongs to the same external subscription lineage and
    // points back at the currently-active one.
    pending.previous_subscription_id = Some(current.id);
    pending.external_id = current.external_id.clone();
    pending.status = SubscriptionStatus::Pending;
    pending.billing_time = current.billing_time;
    pending.subscription_at = current.subscription_at;
    pending.started_at = None;
    pending.terminated_at = None;
    pending.canceled_at = None;
    pending.created_at = now;
    pending.updated_at = now;

    ScheduledDowngrade {
        current_subscription: current.clone(),
        pending_subscription: pending,
    }
}

/// Applied at the next billing boundary (`Subscriptions::PlanUpgradeService` runs
/// for the queued pending subscription): terminate the outgoing subscription and
/// activate the pending one, which now bills its (new) plan for the fresh period.
/// Still no mid-period credit — the previous period was billed in full.
pub fn apply_pending_downgrade(
    current: &Subscription,
    pending: &Subscription,
    boundary: DateTime<Utc>,
) -> (Subscription, Subscription) {
    let mut terminated = current.clone();
    terminated.status = SubscriptionStatus::Terminated;
    terminated.terminated_at = Some(boundary);
    terminated.updated_at = boundary;

    let mut activated = pending.clone();
    activated.status = SubscriptionStatus::Active;
    activated.started_at = Some(boundary);
    activated.updated_at = boundary;

    (terminated, activated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rago_core::enums::{BillingTime, PlanInterval};

    fn plan(amount: i64) -> Plan {
        let now = Utc::now();
        Plan {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            name: "p".into(),
            code: "p".into(),
            interval: Some(PlanInterval::Monthly),
            description: None,
            amount_cents: Some(amount),
            amount_currency: "USD".into(),
            trial_period: None,
            pay_in_advance: false,
            bill_charges_monthly: None,
            parent_id: None,
            invoice_display_name: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn subscription(plan_id: Uuid) -> Subscription {
        let now = Utc::now();
        Subscription {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            customer_id: Uuid::new_v4(),
            plan_id,
            billing_entity_id: None,
            external_id: "ext_1".into(),
            name: None,
            status: SubscriptionStatus::Active,
            billing_time: BillingTime::Anniversary,
            subscription_at: Some(now),
            started_at: Some(now),
            ending_at: None,
            trial_ended_at: None,
            canceled_at: None,
            terminated_at: None,
            previous_subscription_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn schedule_leaves_current_active_and_creates_pending_no_credit() {
        let old_plan = plan(5000);
        let new_plan = plan(2000);
        let current = subscription(old_plan.id);
        let now = Utc::now();

        let res = schedule_downgrade(&current, &new_plan, now);

        // Current is untouched: still active, same plan, no termination/credit.
        assert_eq!(res.current_subscription.status, SubscriptionStatus::Active);
        assert_eq!(res.current_subscription.plan_id, old_plan.id);
        assert!(res.current_subscription.terminated_at.is_none());

        // Pending is on the new plan, pending, same external lineage.
        assert_eq!(res.pending_subscription.status, SubscriptionStatus::Pending);
        assert_eq!(res.pending_subscription.plan_id, new_plan.id);
        assert_eq!(res.pending_subscription.external_id, "ext_1");
        assert_eq!(
            res.pending_subscription.previous_subscription_id,
            Some(current.id)
        );
        assert!(res.pending_subscription.started_at.is_none());
        assert_ne!(res.pending_subscription.id, current.id);
    }

    #[test]
    fn apply_at_boundary_terminates_current_activates_pending() {
        let old_plan = plan(5000);
        let new_plan = plan(2000);
        let current = subscription(old_plan.id);
        let now = Utc::now();
        let scheduled = schedule_downgrade(&current, &new_plan, now);

        let boundary = Utc::now();
        let (terminated, activated) = apply_pending_downgrade(
            &scheduled.current_subscription,
            &scheduled.pending_subscription,
            boundary,
        );

        assert_eq!(terminated.status, SubscriptionStatus::Terminated);
        assert_eq!(terminated.terminated_at, Some(boundary));
        assert_eq!(activated.status, SubscriptionStatus::Active);
        assert_eq!(activated.started_at, Some(boundary));
        assert_eq!(activated.plan_id, new_plan.id);
    }
}

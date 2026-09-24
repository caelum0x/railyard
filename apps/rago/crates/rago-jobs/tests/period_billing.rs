// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Period billing through the real billing run against a migrated Postgres:
//! anniversary quarterly / yearly plans are billed on their rollover day for
//! the right period, pay-in-advance plans bill the closed period's usage with
//! the fee for the period ahead, and a pay-in-advance subscription leaving
//! through a downgrade is billed its closing usage exactly once.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_core::enums::{
    AggregationType, BillingTime, ChargeModel, FeeType, PlanInterval, SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Customer, Event, Organization, Plan, Subscription};
use rago_db::Db;
use rago_integrations::MockTransport;
use rago_jobs::scheduler::{run_billing_cycle, SchedulerConfig};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn no_charge() -> SchedulerConfig {
    SchedulerConfig { charge_customers: false }
}

fn transport() -> MockTransport {
    MockTransport::always(500, "{}")
}

struct World {
    db: Db,
    org: Uuid,
    customer: Uuid,
    metric: BillableMetric,
}

async fn world(pool: PgPool) -> World {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let now = Utc::now();
    db.insert_organization(&Organization {
        id: org,
        name: "Acme".into(),
        slug: format!("acme-{}", &org.to_string()[..8]),
        default_currency: "USD".into(),
        email: None,
        country: None,
        tax_identification_number: None,
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: true,
        hmac_key: Some("test-key".into()),
        created_at: now,
        updated_at: now,
    })
    .await
    .unwrap();
    let customer = Uuid::new_v4();
    db.insert_customer(&Customer {
        id: customer,
        organization_id: org,
        billing_entity_id: org,
        external_id: format!("cust-{}", &customer.to_string()[..8]),
        name: Some("Wile E".into()),
        email: None,
        country: None,
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    })
    .await
    .unwrap();
    let metric = BillableMetric {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "Emails sent".into(),
        code: "emails".into(),
        description: None,
        aggregation_type: AggregationType::SumAgg,
        field_name: Some("value".into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    };
    db.insert_billable_metric(&metric).await.unwrap();
    World { db, org, customer, metric }
}

impl World {
    /// A plan with a flat fee and a 0.25/email usage charge (in arrears).
    async fn plan(&self, interval: PlanInterval, amount_cents: i64, pay_in_advance: bool) -> Plan {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let plan = Plan {
            id,
            organization_id: self.org,
            name: "Pro".into(),
            code: format!("pro-{}", &id.to_string()[..8]),
            interval: Some(interval),
            description: None,
            amount_cents: Some(amount_cents),
            amount_currency: "USD".into(),
            trial_period: None,
            pay_in_advance,
            bill_charges_monthly: None,
            parent_id: None,
            invoice_display_name: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        self.db.insert_plan(&plan).await.unwrap();
        self.db
            .insert_charge(&Charge {
                id: Uuid::new_v4(),
                organization_id: self.org,
                billable_metric_id: Some(self.metric.id),
                plan_id: Some(id),
                code: "emails".into(),
                charge_model: ChargeModel::Standard,
                properties: json!({ "amount": "0.25" }),
                pay_in_advance: false,
                prorated: false,
                invoiceable: true,
                min_amount_cents: 0,
                amount_currency: Some("USD".into()),
                accepts_target_wallet: false,
                invoice_display_name: None,
                parent_id: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .unwrap();
        plan
    }

    #[allow(clippy::too_many_arguments)]
    async fn subscription(
        &self,
        plan: &Plan,
        external_id: &str,
        billing_time: BillingTime,
        start: NaiveDate,
        status: SubscriptionStatus,
        previous: Option<Uuid>,
    ) -> Subscription {
        let now = Utc::now();
        let at = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0).unwrap());
        let sub = Subscription {
            id: Uuid::new_v4(),
            organization_id: self.org,
            customer_id: self.customer,
            plan_id: plan.id,
            billing_entity_id: Some(self.org),
            external_id: external_id.into(),
            name: None,
            status,
            billing_time,
            subscription_at: Some(at),
            started_at: matches!(status, SubscriptionStatus::Active).then_some(at),
            ending_at: None,
            trial_ended_at: None,
            canceled_at: None,
            terminated_at: None,
            previous_subscription_id: previous,
            created_at: now,
            updated_at: now,
        };
        self.db.insert_subscription(&sub).await.unwrap();
        sub
    }

    async fn event(&self, sub: &Subscription, day: NaiveDate, emails: i64) {
        let now = Utc::now();
        self.db
            .insert_event(&Event {
                id: Uuid::new_v4(),
                organization_id: self.org,
                customer_id: Some(self.customer),
                subscription_id: Some(sub.id),
                transaction_id: Uuid::new_v4().to_string(),
                code: "emails".into(),
                properties: json!({ "value": emails }),
                external_customer_id: None,
                external_subscription_id: Some(sub.external_id.clone()),
                timestamp: Some(day.and_hms_opt(12, 0, 0).unwrap()),
                precise_total_amount_cents: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                persisted: true,
            })
            .await
            .unwrap();
    }

    async fn invoices_of(&self, sub: Uuid) -> Vec<(Uuid, i64, NaiveDate, NaiveDate)> {
        sqlx::query_as::<_, (Uuid, i64, chrono::NaiveDateTime, chrono::NaiveDateTime)>(
            "SELECT i.id, i.fees_amount_cents, s.charges_from_datetime, s.charges_to_datetime \
             FROM invoices i JOIN invoice_subscriptions s ON s.invoice_id = i.id \
             WHERE s.subscription_id = $1 ORDER BY s.charges_from_datetime",
        )
        .bind(sub)
        .fetch_all(&self.db.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, cents, from, to)| (id, cents, from.date(), to.date()))
        .collect()
    }

    /// `(subscription fee cents, usage fee cents)` of an invoice.
    async fn split(&self, invoice: Uuid) -> (i64, i64) {
        let fees = self.db.fees_for_invoice(invoice).await.unwrap();
        let sub_fee = fees
            .iter()
            .filter(|f| matches!(f.fee_type, Some(FeeType::Subscription)))
            .map(|f| f.amount_cents)
            .sum();
        let usage = fees
            .iter()
            .filter(|f| !matches!(f.fee_type, Some(FeeType::Subscription)))
            .map(|f| f.amount_cents)
            .sum();
        (sub_fee, usage)
    }
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn anniversary_quarterly_is_billed_on_its_rollover_day(pool: PgPool) {
    let w = world(pool).await;
    let plan = w.plan(PlanInterval::Quarterly, 30_000, false).await;
    let sub = w
        .subscription(&plan, "sub-q", BillingTime::Anniversary, d(2026, 1, 31), SubscriptionStatus::Active, None)
        .await;
    w.event(&sub, d(2026, 3, 10), 100).await; // in the first quarter → 2_500
    w.event(&sub, d(2026, 5, 5), 40).await; // next quarter → not billed yet

    // Not a rollover day (calendar quarter start, day before the anniversary).
    for not_due in [d(2026, 4, 1), d(2026, 4, 29)] {
        let r = run_billing_cycle(&w.db, &transport(), &no_charge(), not_due, false).await.unwrap();
        assert!(r.billed_invoices.is_empty(), "{not_due}");
    }

    // Jan 31 + 3 months clamps to Apr 30.
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 4, 30), false).await.unwrap();
    assert_eq!(r.billed_invoices.len(), 1);
    let invoices = w.invoices_of(sub.id).await;
    assert_eq!(invoices.len(), 1);
    let (id, fees, from, to) = invoices[0];
    assert_eq!((from, to), (d(2026, 1, 31), d(2026, 4, 29)));
    assert_eq!(w.split(id).await, (30_000, 2_500));
    assert_eq!(fees, 32_500);

    // Re-running the same day never bills twice.
    run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 4, 30), false).await.unwrap();
    assert_eq!(w.invoices_of(sub.id).await.len(), 1);

    // Next quarter rolls over on Jul 31 and bills Apr 30 – Jul 30.
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 7, 31), false).await.unwrap();
    assert_eq!(r.billed_invoices.len(), 1);
    let invoices = w.invoices_of(sub.id).await;
    let (id, _, from, to) = invoices[1];
    assert_eq!((from, to), (d(2026, 4, 30), d(2026, 7, 30)));
    assert_eq!(w.split(id).await, (30_000, 1_000));
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn anniversary_yearly_in_advance_bills_fee_ahead_and_usage_behind(pool: PgPool) {
    let w = world(pool).await;
    let plan = w.plan(PlanInterval::Yearly, 120_000, true).await;
    let sub = w
        .subscription(&plan, "sub-y", BillingTime::Anniversary, d(2025, 3, 5), SubscriptionStatus::Active, None)
        .await;
    w.event(&sub, d(2025, 6, 1), 40).await; // first year's usage → 1_000

    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 3, 4), false).await.unwrap();
    assert!(r.billed_invoices.is_empty());
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 3, 5), false).await.unwrap();
    assert_eq!(r.billed_invoices.len(), 1);

    let invoices = w.invoices_of(sub.id).await;
    let (id, fees, from, to) = invoices[0];
    // Keyed by the paid-ahead year; the fee covers it, the usage is last year's.
    assert_eq!((from, to), (d(2026, 3, 5), d(2027, 3, 4)));
    assert_eq!(w.split(id).await, (120_000, 1_000));
    assert_eq!(fees, 121_000);
    let issuing: NaiveDate = sqlx::query_scalar("SELECT issuing_date FROM invoices WHERE id = $1")
        .bind(id)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(issuing, d(2026, 3, 5), "issued on the billing day, not the end of the paid-ahead year");
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn downgrade_from_pay_in_advance_bills_closing_usage_once(pool: PgPool) {
    let w = world(pool).await;
    let pro = w.plan(PlanInterval::Monthly, 5_000, true).await;
    let basic = w.plan(PlanInterval::Monthly, 2_000, false).await;
    let outgoing = w
        .subscription(&pro, "sub-dg", BillingTime::Calendar, d(2026, 1, 1), SubscriptionStatus::Active, None)
        .await;
    let incoming = w
        .subscription(
            &basic,
            "sub-dg",
            BillingTime::Calendar,
            d(2026, 3, 1),
            SubscriptionStatus::Pending,
            Some(outgoing.id),
        )
        .await;
    w.event(&outgoing, d(2026, 2, 10), 40).await; // February usage → 1_000
    w.event(&outgoing, d(2026, 3, 10), 20).await; // March, on the new plan → 500

    // The billing run on the boundary must not bill the leaving pay-in-advance
    // subscription the next month's fee.
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 3, 1), false).await.unwrap();
    assert!(r.billed_invoices.is_empty());
    assert!(w.invoices_of(outgoing.id).await.is_empty());

    // The swap bills February's usage on a closing invoice (no fee line).
    let applied = rago_jobs::clock::subscriptions::apply_scheduled_downgrades(&w.db).await.unwrap();
    assert_eq!(applied, 1);
    let closing = w.invoices_of(outgoing.id).await;
    assert_eq!(closing.len(), 1);
    let (id, fees, from, to) = closing[0];
    assert_eq!((from, to), (d(2026, 2, 1), d(2026, 2, 28)));
    assert_eq!(w.split(id).await, (0, 1_000));
    assert_eq!(fees, 1_000);
    let fee_lines = w.db.fees_for_invoice(id).await.unwrap();
    assert!(fee_lines.iter().all(|f| !matches!(f.fee_type, Some(FeeType::Subscription))));

    // Idempotent: billing the closing usage again returns the same invoice.
    let again = rago_jobs::scheduler::bill_closing_charges_and_collect(
        &w.db,
        &transport(),
        &outgoing,
        &pro,
        d(2026, 3, 1),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(again.invoice_id, id);
    assert!(!again.created);
    assert_eq!(w.invoices_of(outgoing.id).await.len(), 1);

    // The incoming (arrears) plan bills only March's usage on Apr 1 — never
    // the outgoing plan's February events that share its external id.
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 4, 1), false).await.unwrap();
    assert_eq!(r.billed_invoices.len(), 1);
    let inc = w.invoices_of(incoming.id).await;
    assert_eq!(inc.len(), 1);
    assert_eq!(w.split(inc[0].0).await, (2_000, 500));
}

async fn end_at(w: &World, sub: Uuid, boundary: NaiveDate) {
    sqlx::query("UPDATE subscriptions SET ending_at = $2, cancel_at_period_end = true WHERE id = $1")
        .bind(sub)
        .bind(boundary.and_hms_opt(0, 0, 0).unwrap())
        .execute(&w.db.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn cancel_at_period_end_bills_the_final_period_before_terminating(pool: PgPool) {
    let w = world(pool).await;
    let plan = w.plan(PlanInterval::Monthly, 2_900, false).await;
    let sub = w
        .subscription(&plan, "sub-end", BillingTime::Calendar, d(2026, 1, 1), SubscriptionStatus::Active, None)
        .await;
    w.event(&sub, d(2026, 2, 10), 40).await; // → 1_000
    end_at(&w, sub.id, d(2026, 3, 1)).await;

    // The termination clock runs before any billing run on the boundary: the
    // closing month is still billed, then the subscription terminates.
    let n = rago_jobs::clock::subscriptions::terminate_ended_subscriptions(&w.db).await.unwrap();
    assert_eq!(n, 1);
    let invoices = w.invoices_of(sub.id).await;
    assert_eq!(invoices.len(), 1);
    let (id, _, from, to) = invoices[0];
    assert_eq!((from, to), (d(2026, 2, 1), d(2026, 2, 28)));
    assert_eq!(w.split(id).await, (2_900, 1_000));
    let status: i32 = sqlx::query_scalar("SELECT status FROM subscriptions WHERE id = $1")
        .bind(sub.id)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(status, 2);
    // Idempotent.
    assert_eq!(rago_jobs::clock::subscriptions::terminate_ended_subscriptions(&w.db).await.unwrap(), 0);
    assert_eq!(w.invoices_of(sub.id).await.len(), 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn cancel_at_period_end_in_advance_bills_closing_usage_not_next_fee(pool: PgPool) {
    let w = world(pool).await;
    let plan = w.plan(PlanInterval::Monthly, 5_000, true).await;
    let sub = w
        .subscription(&plan, "sub-end-adv", BillingTime::Calendar, d(2026, 1, 1), SubscriptionStatus::Active, None)
        .await;
    w.event(&sub, d(2026, 2, 10), 40).await; // → 1_000
    end_at(&w, sub.id, d(2026, 3, 1)).await;

    // The billing run on the last boundary does not bill March's fee.
    let r = run_billing_cycle(&w.db, &transport(), &no_charge(), d(2026, 3, 1), false).await.unwrap();
    assert!(r.billed_invoices.is_empty());

    rago_jobs::clock::subscriptions::terminate_ended_subscriptions(&w.db).await.unwrap();
    let invoices = w.invoices_of(sub.id).await;
    assert_eq!(invoices.len(), 1);
    let (id, _, from, to) = invoices[0];
    assert_eq!((from, to), (d(2026, 2, 1), d(2026, 2, 28)));
    assert_eq!(w.split(id).await, (0, 1_000));
}

// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Charge filters, pricing groups and prorated charges through the real
//! billing run on a migrated Postgres. Before this, the run read every event
//! of the metric and priced it with the charge's base properties: a filtered
//! plan billed every event at the default price, groups were one line and a
//! prorated seat charge billed full seats.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_core::enums::{AggregationType, BillingTime, ChargeModel, FeeType, PlanInterval, SubscriptionStatus};
use rago_core::models::{BillableMetric, Charge, Customer, Event, Fee, Organization, Plan, Subscription};
use rago_db::charge_filters::ChargeFilterSpec;
use rago_db::Db;
use rago_integrations::MockTransport;
use rago_jobs::scheduler::{run_billing_cycle, SchedulerConfig};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

struct World {
    db: Db,
    org: Uuid,
    customer: Uuid,
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
    World { db, org, customer }
}

impl World {
    async fn metric(&self, code: &str, agg: AggregationType, field: Option<&str>, recurring: bool) -> BillableMetric {
        let now = Utc::now();
        let m = BillableMetric {
            id: Uuid::new_v4(),
            organization_id: self.org,
            name: code.into(),
            code: code.into(),
            description: None,
            aggregation_type: agg,
            field_name: field.map(str::to_string),
            recurring,
            expression: None,
            rounding_function: None,
            rounding_precision: None,
            custom_aggregator: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        self.db.insert_billable_metric(&m).await.unwrap();
        m
    }

    async fn plan(&self) -> Plan {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let plan = Plan {
            id,
            organization_id: self.org,
            name: "Pro".into(),
            code: format!("pro-{}", &id.to_string()[..8]),
            interval: Some(PlanInterval::Monthly),
            description: None,
            amount_cents: Some(0),
            amount_currency: "USD".into(),
            trial_period: None,
            pay_in_advance: false,
            bill_charges_monthly: None,
            parent_id: None,
            invoice_display_name: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        self.db.insert_plan(&plan).await.unwrap();
        plan
    }

    async fn charge(
        &self,
        plan: &Plan,
        m: &BillableMetric,
        properties: Value,
        prorated: bool,
        in_advance: bool,
    ) -> Charge {
        let now = Utc::now();
        let c = Charge {
            id: Uuid::new_v4(),
            organization_id: self.org,
            billable_metric_id: Some(m.id),
            plan_id: Some(plan.id),
            code: m.code.clone(),
            charge_model: ChargeModel::Standard,
            properties,
            pay_in_advance: in_advance,
            prorated,
            invoiceable: true,
            min_amount_cents: 0,
            amount_currency: Some("USD".into()),
            accepts_target_wallet: false,
            invoice_display_name: None,
            parent_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        self.db.insert_charge(&c).await.unwrap();
        c
    }

    async fn subscription(&self, plan: &Plan, external_id: &str, start: NaiveDate) -> Subscription {
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
            status: SubscriptionStatus::Active,
            billing_time: BillingTime::Calendar,
            subscription_at: Some(at),
            started_at: Some(at),
            ending_at: None,
            trial_ended_at: None,
            canceled_at: None,
            terminated_at: None,
            previous_subscription_id: None,
            created_at: now,
            updated_at: now,
        };
        self.db.insert_subscription(&sub).await.unwrap();
        sub
    }

    async fn event(&self, sub: &Subscription, code: &str, day: NaiveDate, props: Value) -> Uuid {
        let now = Utc::now();
        let id = Uuid::new_v4();
        self.db
            .insert_event(&Event {
                id,
                organization_id: self.org,
                customer_id: Some(self.customer),
                subscription_id: Some(sub.id),
                transaction_id: Uuid::new_v4().to_string(),
                code: code.into(),
                properties: props,
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
        id
    }

    async fn usage_fees(&self, sub: &Subscription) -> Vec<Fee> {
        let invoice: Uuid = sqlx::query_scalar(
            "SELECT i.id FROM invoices i JOIN invoice_subscriptions s ON s.invoice_id = i.id \
             WHERE s.subscription_id = $1 ORDER BY s.charges_from_datetime LIMIT 1",
        )
        .bind(sub.id)
        .fetch_one(&self.db.pool)
        .await
        .unwrap();
        self.db
            .fees_for_invoice(invoice)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| !matches!(f.fee_type, Some(FeeType::Subscription)))
            .collect()
    }
}

fn no_charge() -> SchedulerConfig {
    SchedulerConfig {
        charge_customers: false,
    }
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn the_billing_run_prices_each_filter_and_group_on_its_own_line(pool: PgPool) {
    let w = world(pool).await;
    let m = w.metric("gb", AggregationType::SumAgg, Some("gb"), false).await;
    let plan = w.plan().await;
    let c = w.charge(&plan, &m, json!({ "amount": "1" }), false, false).await;
    w.db.replace_charge_filters(
        w.org,
        c.id,
        &[
            ChargeFilterSpec {
                invoice_display_name: Some("EU".into()),
                properties: json!({ "amount": "3" }),
                values: json!({ "region": ["eu"] }),
            },
            ChargeFilterSpec {
                invoice_display_name: Some("US".into()),
                properties: json!({ "amount": "2", "pricing_group_keys": ["team"] }),
                values: json!({ "region": ["us"] }),
            },
        ],
    )
    .await
    .unwrap();
    let filters = w.db.charge_filter_rows(c.id).await.unwrap();
    let eu = filters
        .iter()
        .find(|f| f.spec.invoice_display_name.as_deref() == Some("EU"))
        .unwrap()
        .id;
    let us = filters
        .iter()
        .find(|f| f.spec.invoice_display_name.as_deref() == Some("US"))
        .unwrap()
        .id;

    let sub = w.subscription(&plan, "sub-f", d(2026, 5, 1)).await;
    w.event(&sub, "gb", d(2026, 5, 3), json!({ "gb": 4, "region": "eu" }))
        .await;
    w.event(
        &sub,
        "gb",
        d(2026, 5, 4),
        json!({ "gb": 2, "region": "us", "team": "a" }),
    )
    .await;
    w.event(
        &sub,
        "gb",
        d(2026, 5, 5),
        json!({ "gb": 3, "region": "us", "team": "b" }),
    )
    .await;
    w.event(&sub, "gb", d(2026, 5, 6), json!({ "gb": 5, "region": "asia" }))
        .await;

    let r = run_billing_cycle(
        &w.db,
        &MockTransport::always(500, "{}"),
        &no_charge(),
        d(2026, 6, 1),
        false,
    )
    .await
    .unwrap();
    assert_eq!(r.billed_invoices.len(), 1);
    let fees = w.usage_fees(&sub).await;
    let line = |filter: Option<Uuid>, group: Value| {
        fees.iter()
            .find(|f| f.charge_filter_id == filter && f.grouped_by == group)
            .map(|f| (f.units, f.amount_cents))
            .unwrap_or_else(|| panic!("no line {filter:?} {group}: {fees:?}"))
    };
    assert_eq!(line(Some(eu), json!({})), (4.into(), 1_200));
    assert_eq!(line(Some(us), json!({ "team": "a" })), (2.into(), 400));
    assert_eq!(line(Some(us), json!({ "team": "b" })), (3.into(), 600));
    assert_eq!(line(None, json!({})), (5.into(), 500));
    assert_eq!(fees.len(), 4);
    // Before: one line of 14 GB at the default $1 = 1_400.
    assert_eq!(fees.iter().map(|f| f.amount_cents).sum::<i64>(), 2_700);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn a_prorated_recurring_seat_charge_bills_seats_for_their_days(pool: PgPool) {
    let w = world(pool).await;
    let m = w.metric("seats", AggregationType::SumAgg, Some("seats"), true).await;
    let plan = w.plan().await;
    w.charge(&plan, &m, json!({ "amount": "10" }), true, false).await;
    let sub = w.subscription(&plan, "sub-p", d(2026, 4, 1)).await;
    w.event(&sub, "seats", d(2026, 4, 10), json!({ "seats": 10 })).await; // carried into June
    w.event(&sub, "seats", d(2026, 6, 16), json!({ "seats": 30 })).await; // 15 of 30 June days

    // June is billed on July 1 (the earlier months are billed too, first).
    let r = run_billing_cycle(
        &w.db,
        &MockTransport::always(500, "{}"),
        &no_charge(),
        d(2026, 7, 1),
        false,
    )
    .await
    .unwrap();
    assert!(!r.billed_invoices.is_empty());
    let june: Uuid = sqlx::query_scalar(
        "SELECT i.id FROM invoices i JOIN invoice_subscriptions s ON s.invoice_id = i.id \
         WHERE s.subscription_id = $1 AND s.charges_from_datetime::date = '2026-06-01'",
    )
    .bind(sub.id)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    let fees: Vec<Fee> =
        w.db.fees_for_invoice(june)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| !matches!(f.fee_type, Some(FeeType::Subscription)))
            .collect();
    assert_eq!(fees.len(), 1);
    // 10 seats * 30/30 + 30 seats * 15/30 = 25 seat-months at $10; units show 40 seats.
    assert_eq!(fees[0].amount_cents, 25_000);
    assert_eq!(fees[0].units, 40.into());
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn a_pay_in_advance_event_is_billed_at_its_filter_price(pool: PgPool) {
    let w = world(pool).await;
    let m = w.metric("calls", AggregationType::CountAgg, None, false).await;
    let plan = w.plan().await;
    let c = w.charge(&plan, &m, json!({ "amount": "1" }), false, true).await;
    w.db.replace_charge_filters(
        w.org,
        c.id,
        &[ChargeFilterSpec {
            invoice_display_name: None,
            properties: json!({ "amount": "4" }),
            values: json!({ "model": ["large"] }),
        }],
    )
    .await
    .unwrap();
    let sub = w.subscription(&plan, "sub-a", d(2026, 1, 1)).await;
    let today = Utc::now().date_naive();
    let small = w.event(&sub, "calls", today, json!({ "model": "small" })).await;
    let large = w.event(&sub, "calls", today, json!({ "model": "large" })).await;

    let a = rago_jobs::pay_in_advance::bill_event_in_advance(&w.db, small)
        .await
        .unwrap()
        .expect("small billed");
    let b = rago_jobs::pay_in_advance::bill_event_in_advance(&w.db, large)
        .await
        .unwrap()
        .expect("large billed");
    assert_eq!(a.fees_amount_cents, 100, "the default price");
    assert_eq!(b.fees_amount_cents, 400, "the filter's price, not the default");
    let fee = w.db.fees_for_invoice(b.id).await.unwrap();
    assert!(fee[0].charge_filter_id.is_some());
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn fixed_charges_are_billed_in_arrears_prorated_and_upfront(pool: PgPool) {
    let w = world(pool).await;
    // Pay in arrears: 5 seats at $12, prorated, from May 17 (15 of 31 days),
    // and a $50 platform unit, not prorated.
    let plan = w.plan().await;
    let seats = Uuid::new_v4();
    w.db.insert_fixed_charge(seats, w.org, plan.id, None, 0, &json!({ "amount": "12" }), 5.into(), false, true, Some("Seats"))
        .await
        .unwrap();
    let platform = Uuid::new_v4();
    w.db.insert_fixed_charge(platform, w.org, plan.id, None, 0, &json!({ "amount": "50" }), 1.into(), false, false, None)
        .await
        .unwrap();
    let sub = w.subscription(&plan, "sub-fc", d(2026, 5, 17)).await;
    run_billing_cycle(&w.db, &MockTransport::always(500, "{}"), &no_charge(), d(2026, 6, 1), false).await.unwrap();
    let fees = w.usage_fees(&sub).await;
    let of = |id: Uuid| fees.iter().find(|f| f.invoiceable_id == Some(id)).unwrap_or_else(|| panic!("{fees:?}"));
    // $60 * 15/31 = 29.03; units show the 5 seats.
    assert_eq!(of(seats).amount_cents, 2_903);
    assert_eq!(of(seats).units, 5.into());
    assert!(matches!(of(seats).fee_type, Some(FeeType::FixedCharge)));
    assert_eq!(of(platform).amount_cents, 5_000);

    // Pay in advance: an upfront fixed charge is billed with the fee for the
    // period ahead; a re-run bills nothing again.
    let ahead = w.plan().await;
    sqlx::query("UPDATE plans SET pay_in_advance = true WHERE id = $1").bind(ahead.id).execute(&w.db.pool).await.unwrap();
    let licence = Uuid::new_v4();
    w.db.insert_fixed_charge(licence, w.org, ahead.id, None, 0, &json!({ "amount": "7" }), 3.into(), true, false, None)
        .await
        .unwrap();
    let sub2 = w.subscription(&ahead, "sub-fc2", d(2026, 6, 1)).await;
    for _ in 0..2 {
        run_billing_cycle(&w.db, &MockTransport::always(500, "{}"), &no_charge(), d(2026, 6, 1), false).await.unwrap();
    }
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT f.amount_cents FROM fees f JOIN invoice_subscriptions s ON s.invoice_id = f.invoice_id \
         WHERE s.subscription_id = $1 AND f.invoiceable_id = $2",
    )
    .bind(sub2.id)
    .bind(licence)
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(2_100,)], "billed once, upfront for June");
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn a_draft_refresh_bills_late_events_of_its_window(pool: PgPool) {
    let w = world(pool).await;
    let m = w.metric("emails", AggregationType::SumAgg, Some("value"), false).await;
    let plan = w.plan().await;
    w.charge(&plan, &m, json!({ "amount": "0.25" }), false, false).await;
    // A 3-day grace period keeps the June 1 invoice a draft.
    w.db.update_customer_invoice_grace_period(w.customer, Some(3)).await.unwrap();
    let sub = w.subscription(&plan, "sub-draft", d(2026, 5, 1)).await;
    w.event(&sub, "emails", d(2026, 5, 10), json!({ "value": 100 })).await;
    run_billing_cycle(&w.db, &MockTransport::always(500, "{}"), &no_charge(), d(2026, 6, 1), false).await.unwrap();
    let draft: Uuid = sqlx::query_scalar(
        "SELECT i.id FROM invoices i JOIN invoice_subscriptions s ON s.invoice_id = i.id WHERE s.subscription_id = $1",
    )
    .bind(sub.id)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(w.db.find_invoice(draft).await.unwrap().unwrap().fees_amount_cents, 2_500);

    // A May event arrives during the grace period; another one for June does not count.
    w.event(&sub, "emails", d(2026, 5, 31), json!({ "value": 20 })).await;
    w.event(&sub, "emails", d(2026, 6, 2), json!({ "value": 999 })).await;
    let changed = rago_jobs::clock::invoices::refresh_draft_invoices(&w.db).await.unwrap();
    assert_eq!(changed, 1);
    let inv = w.db.find_invoice(draft).await.unwrap().unwrap();
    assert_eq!(inv.fees_amount_cents, 3_000, "late May usage billed, June usage not");
    assert_eq!(inv.total_amount_cents, 3_000);
    let usage = w.usage_fees(&sub).await;
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].units, 120.into());

    // Nothing new: the next sweep changes nothing.
    assert_eq!(rago_jobs::clock::invoices::refresh_draft_invoices(&w.db).await.unwrap(), 0);

    // A finalized invoice is never re-priced.
    sqlx::query("UPDATE invoices SET status = 1 WHERE id = $1").bind(draft).execute(&w.db.pool).await.unwrap();
    w.event(&sub, "emails", d(2026, 5, 30), json!({ "value": 40 })).await;
    let (outcome, after) = rago_jobs::refresh_draft::refresh_draft_invoice(&w.db, draft).await.unwrap();
    assert_eq!(outcome, rago_jobs::refresh_draft::Refresh::NotDraft);
    assert_eq!(after.unwrap().fees_amount_cents, 3_000);
}

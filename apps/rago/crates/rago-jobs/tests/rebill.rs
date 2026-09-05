//! End-to-end re-bill idempotency (audit fix #3), through the real
//! `bill_subscription::perform` path against a migrated Postgres.
//!
//! Billing the same subscription for the same period twice must produce exactly
//! one invoice — the second run returns the already-billed invoice rather than
//! charging/invoicing again.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_core::enums::{BillingTime, PlanInterval, SubscriptionStatus};
use rago_core::models::{Customer, Organization, Plan, Subscription};
use rago_db::Db;
use sqlx::PgPool;
use uuid::Uuid;

fn organization(id: Uuid) -> Organization {
    let now = Utc::now();
    Organization {
        id,
        name: "Acme".into(),
        slug: format!("acme-{}", &id.to_string()[..8]),
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
    }
}

fn customer(id: Uuid, org: Uuid) -> Customer {
    let now = Utc::now();
    Customer {
        id,
        organization_id: org,
        billing_entity_id: org,
        external_id: format!("cust-{}", &id.to_string()[..8]),
        name: Some("Wile E".into()),
        email: None,
        country: None,
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn plan(id: Uuid, org: Uuid) -> Plan {
    let now = Utc::now();
    Plan {
        id,
        organization_id: org,
        name: "Pro".into(),
        code: format!("pro-{}", &id.to_string()[..8]),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(1000),
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

fn subscription(id: Uuid, org: Uuid, customer_id: Uuid, plan_id: Uuid) -> Subscription {
    let now = Utc::now();
    Subscription {
        id,
        organization_id: org,
        customer_id,
        plan_id,
        billing_entity_id: Some(org),
        external_id: format!("sub-{}", &id.to_string()[..8]),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: BillingTime::Calendar,
        subscription_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        started_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn perform_is_idempotent_for_same_period(pool: PgPool) {
    let db = Db::new(pool);

    let org = Uuid::new_v4();
    let cust = Uuid::new_v4();
    let pln = Uuid::new_v4();
    let sub = Uuid::new_v4();

    db.insert_organization(&organization(org)).await.unwrap();
    db.insert_customer(&customer(cust, org)).await.unwrap();
    db.insert_plan(&plan(pln, org)).await.unwrap();
    db.insert_subscription(&subscription(sub, org, cust, pln)).await.unwrap();

    let from = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    let to = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap().and_hms_opt(23, 59, 59).unwrap();

    let run1 = rago_jobs::bill_subscription(&db, sub, from, to).await.unwrap();
    let run2 = rago_jobs::bill_subscription(&db, sub, from, to).await.unwrap();

    assert_eq!(
        run1.invoice.id, run2.invoice.id,
        "re-billing the same period returns the same invoice"
    );

    let invoices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id = $1")
        .bind(cust)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(invoices, 1, "exactly one invoice for the subscription+period");
}

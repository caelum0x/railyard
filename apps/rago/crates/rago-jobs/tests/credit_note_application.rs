// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Credit-note balances consumed by later invoices, draft credit notes
//! finalized with their invoice, and interval-aware backfills — through the
//! real billing / finalization paths against a migrated Postgres.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_core::enums::{BillingTime, InvoiceStatus, InvoiceType, PaymentStatus, PlanInterval, SubscriptionStatus};
use rago_core::models::{CreditNote, Customer, Invoice, Organization, Plan, Subscription};
use rago_db::Db;
use rust_decimal::Decimal;
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

fn plan(id: Uuid, org: Uuid, interval: PlanInterval, amount_cents: i64) -> Plan {
    let now = Utc::now();
    Plan {
        id,
        organization_id: org,
        name: "Pro".into(),
        code: format!("pro-{}", &id.to_string()[..8]),
        interval: Some(interval),
        description: None,
        amount_cents: Some(amount_cents),
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


struct Fixture {
    db: Db,
    org: Uuid,
    customer: Uuid,
    subscription: Uuid,
}

async fn setup(pool: PgPool, interval: PlanInterval, amount_cents: i64) -> Fixture {
    let db = Db::new(pool);
    let org = Uuid::new_v4();
    let cust = Uuid::new_v4();
    let pln = Uuid::new_v4();
    let sub = Uuid::new_v4();
    db.insert_organization(&organization(org)).await.unwrap();
    db.insert_customer(&customer(cust, org)).await.unwrap();
    db.insert_plan(&plan(pln, org, interval, amount_cents)).await.unwrap();
    db.insert_subscription(&subscription(sub, org, cust, pln)).await.unwrap();
    Fixture { db, org, customer: cust, subscription: sub }
}

/// A finalized one-off invoice of the customer, paid or not.
async fn source_invoice(f: &Fixture, number: &str, paid: bool, status: InvoiceStatus) -> Invoice {
    let now = Utc::now();
    let inv = Invoice {
        id: Uuid::new_v4(),
        organization_id: f.org,
        billing_entity_id: f.org,
        customer_id: Some(f.customer),
        number: number.into(),
        sequential_id: None,
        invoice_type: InvoiceType::OneOff,
        status,
        payment_status: if paid { PaymentStatus::Succeeded } else { PaymentStatus::Pending },
        currency: Some("USD".into()),
        issuing_date: Some(now.date_naive()),
        fees_amount_cents: 5_000,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 5_000,
        sub_total_including_taxes_amount_cents: 5_000,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 5_000,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    };
    f.db.insert_invoice(&inv).await.unwrap();
    inv
}

async fn credit_note(f: &Fixture, invoice: &Invoice, seq: i32, balance: i64, issued: NaiveDate, status: i32) -> Uuid {
    let now = Utc::now();
    let cn = CreditNote {
        id: Uuid::new_v4(),
        organization_id: f.org,
        customer_id: f.customer,
        invoice_id: invoice.id,
        sequential_id: seq,
        number: format!("{}-CN{seq}", if invoice.number.is_empty() { "DRAFT" } else { &invoice.number }),
        issuing_date: issued,
        reason: 0,
        credit_status: Some(0),
        refund_status: None,
        status,
        credit_amount_cents: balance,
        credit_amount_currency: "USD".into(),
        balance_amount_cents: balance,
        refund_amount_cents: 0,
        total_amount_cents: balance,
        total_amount_currency: "USD".into(),
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        coupons_adjustment_amount_cents: 0,
        precise_coupons_adjustment_amount_cents: Decimal::ZERO,
        precise_taxes_amount_cents: Decimal::ZERO,
        created_at: now,
        updated_at: now,
        voided_at: None,
        items: Vec::new(),
        applied_taxes: Vec::new(),
    };
    f.db.insert_credit_note(&cn).await.unwrap();
    cn.id
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn window(from: NaiveDate, to: NaiveDate) -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    (from.and_hms_opt(0, 0, 0).unwrap(), to.and_hms_opt(23, 59, 59).unwrap())
}

async fn note_state(db: &Db, id: Uuid) -> (i64, Option<i32>) {
    let n = db.find_credit_note(id).await.unwrap().unwrap();
    (n.balance_amount_cents, n.credit_status)
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn finalized_invoice_consumes_credit_notes_oldest_first_and_partially(pool: PgPool) {
    let f = setup(pool, PlanInterval::Monthly, 1_000).await;
    let src = source_invoice(&f, "SRC-1", true, InvoiceStatus::Finalized).await;
    // Created newest-first to prove ordering follows the issuing date.
    let newer = credit_note(&f, &src, 2, 1_500, d(2025, 12, 20), 1).await;
    let older = credit_note(&f, &src, 1, 300, d(2025, 12, 1), 1).await;

    let (from, to) = window(d(2026, 1, 1), d(2026, 1, 31));
    let run = rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.unwrap();
    assert_eq!(run.invoice.status, InvoiceStatus::Finalized);
    assert_eq!(run.invoice.credit_notes_amount_cents, 1_000);
    assert_eq!(run.invoice.total_amount_cents, 0);
    assert_eq!(run.invoice.payment_status, PaymentStatus::Succeeded);

    assert_eq!(note_state(&f.db, older).await, (0, Some(1)), "older note consumed first, in full");
    assert_eq!(note_state(&f.db, newer).await, (800, Some(0)), "newer note partially drawn, still available");

    let credits = f.db.credit_note_credits_for_invoice(run.invoice.id).await.unwrap();
    assert_eq!(credits.len(), 2);
    assert_eq!(credits.iter().map(|c| c.amount_cents).sum::<i64>(), 1_000);

    // Re-billing and re-applying never spends a balance twice.
    let again = rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.unwrap();
    assert_eq!(again.invoice.id, run.invoice.id);
    assert!(f.db.apply_available_credit_notes(run.invoice.id).await.unwrap().is_none());
    assert_eq!(note_state(&f.db, newer).await, (800, Some(0)));

    // Voiding the invoice gives the consumed balance back.
    f.db.void_invoice(run.invoice.id).await.unwrap();
    assert_eq!(note_state(&f.db, older).await, (300, Some(0)));
    assert_eq!(note_state(&f.db, newer).await, (1_500, Some(0)));
    // A second void restores nothing more.
    f.db.void_invoice(run.invoice.id).await.unwrap();
    assert_eq!(note_state(&f.db, newer).await, (1_500, Some(0)));
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn notes_on_unpaid_or_draft_invoices_are_not_spent(pool: PgPool) {
    let f = setup(pool, PlanInterval::Monthly, 1_000).await;
    let unpaid = source_invoice(&f, "SRC-UNPAID", false, InvoiceStatus::Finalized).await;
    let unpaid_note = credit_note(&f, &unpaid, 1, 400, d(2025, 12, 1), 1).await;
    let draft = source_invoice(&f, "", true, InvoiceStatus::Draft).await;
    let draft_note = credit_note(&f, &draft, 1, 400, d(2025, 12, 1), 0).await;

    let (from, to) = window(d(2026, 1, 1), d(2026, 1, 31));
    let run = rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.unwrap();
    assert_eq!(run.invoice.credit_notes_amount_cents, 0);
    assert_eq!(run.invoice.total_amount_cents, 1_000);
    assert_eq!(note_state(&f.db, unpaid_note).await, (400, Some(0)));
    assert_eq!(note_state(&f.db, draft_note).await, (400, Some(0)));

    // Draft credit notes stay out of the credit-note lists.
    let listed = f.db.credit_notes_for_org(f.org, 50, 0).await.unwrap();
    assert!(listed.iter().all(|n| n.id != draft_note));
    assert!(listed.iter().any(|n| n.id == unpaid_note));
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn draft_credit_note_is_finalized_and_renumbered_with_its_invoice(pool: PgPool) {
    let f = setup(pool, PlanInterval::Monthly, 1_000).await;
    let draft = source_invoice(&f, "", true, InvoiceStatus::Draft).await;
    let note = credit_note(&f, &draft, 1, 250, d(2026, 1, 10), 0).await;

    let finalized = rago_jobs::finalize::finalize_invoice(&f.db, draft.id).await.unwrap().unwrap();
    assert!(!finalized.number.is_empty());
    let cn = f.db.find_credit_note(note).await.unwrap().unwrap();
    assert_eq!(cn.status, 1);
    assert_eq!(cn.number, format!("{}-CN1", finalized.number));
    // Its own invoice does not consume it.
    assert_eq!(cn.balance_amount_cents, 250);
    assert_eq!(finalized.credit_notes_amount_cents, 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn quarterly_backfill_bills_the_quarterly_fee(pool: PgPool) {
    let f = setup(pool, PlanInterval::Quarterly, 9_000).await;

    let (from, to) = window(d(2026, 1, 1), d(2026, 3, 31));
    let full = rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.unwrap();
    assert_eq!(full.invoice.fees_amount_cents, 9_000, "a whole quarter bills the full quarterly fee");

    // One month of the next quarter (Q2 2026 = 91 days; April = 30 days).
    let (from, to) = window(d(2026, 4, 1), d(2026, 4, 30));
    let month = rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.unwrap();
    assert_eq!(month.invoice.fees_amount_cents, 2_967, "30/91 of the quarterly fee");

    // A window crossing the quarter boundary is refused.
    let (from, to) = window(d(2026, 6, 1), d(2026, 7, 31));
    assert!(rago_jobs::bill_subscription(&f.db, f.subscription, from, to).await.is_err());
}

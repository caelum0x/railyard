// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Real-time wallet debit through Mint holds, against a real Postgres and a
//! scripted (fake) Mint transport: reserve per event with an idempotent key,
//! reject or flag on insufficient funds, capture at invoice finalization.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Utc};
use rago_core::enums::{
    AggregationType, BillingTime, ChargeModel, PlanInterval, SubscriptionStatus, WalletStatus,
};
use rago_core::models::{BillableMetric, Charge, Customer, Event, Organization, Plan, Subscription, Wallet};
use rago_db::Db;
use rago_integrations::{HttpResponse, MockTransport};
use rago_jobs::wallet_realtime::{reserve_for_event, settle_invoice, Reservation};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn month_start() -> NaiveDateTime {
    let t = Utc::now().date_naive();
    NaiveDate::from_ymd_opt(t.year(), t.month(), 1).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

fn month_end() -> NaiveDateTime {
    let s = month_start().date();
    let next = if s.month() == 12 {
        NaiveDate::from_ymd_opt(s.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(s.year(), s.month() + 1, 1).unwrap()
    };
    next.pred_opt().unwrap().and_hms_opt(23, 59, 59).unwrap()
}

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
        subscription_at: Some(month_start().and_utc()),
        started_at: Some(month_start().and_utc()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn wallet(id: Uuid, org: Uuid, customer_id: Uuid, balance_cents: i64) -> Wallet {
    let now = Utc::now();
    Wallet {
        id,
        organization_id: org,
        customer_id,
        status: WalletStatus::Active,
        name: None,
        rate_amount: Decimal::ONE,
        credits_balance: Decimal::from(balance_cents) / Decimal::from(100),
        consumed_credits: Decimal::ZERO,
        balance_cents,
        balance_currency: "USD".into(),
        allowed_fee_types: Vec::new(),
        consumed_amount_cents: 0,
        ongoing_balance_cents: 0,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 50,
        created_at: now,
        updated_at: now,
    }
}


fn metric(org: Uuid) -> BillableMetric {
    let now = Utc::now();
    BillableMetric {
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
    }
}

fn charge(org: Uuid, plan_id: Uuid, metric_id: Uuid) -> Charge {
    let now = Utc::now();
    Charge {
        id: Uuid::new_v4(),
        organization_id: org,
        billable_metric_id: Some(metric_id),
        plan_id: Some(plan_id),
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
    }
}

fn event(org: Uuid, sub: &Subscription, tx: &str, value: i64) -> Event {
    let now = Utc::now();
    Event {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Some(sub.customer_id),
        subscription_id: Some(sub.id),
        transaction_id: tx.into(),
        code: "emails".into(),
        properties: json!({ "value": value }),
        external_customer_id: None,
        external_subscription_id: Some(sub.external_id.clone()),
        timestamp: Some(now.naive_utc()),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

fn ok(body: &str) -> Result<HttpResponse, rago_integrations::TransportError> {
    Ok(HttpResponse { status: 200, body: body.into() })
}

struct World {
    db: Db,
    org: Uuid,
    sub: Subscription,
    wallet: Uuid,
}

async fn world(pool: PgPool, policy: &str) -> World {
    std::env::set_var("RAGO_MINT_URL", "https://mint.test");
    std::env::set_var("RAGO_MINT_API_KEY", "mint-key");
    std::env::set_var("RAGO_MINT_PAYEE_ACCOUNT", "acct_seller");
    let db = Db::new(pool);
    let (org, cust, pln, sub_id, wal) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    db.insert_organization(&organization(org)).await.unwrap();
    db.insert_customer(&customer(cust, org)).await.unwrap();
    db.insert_plan(&plan(pln, org)).await.unwrap();
    let m = metric(org);
    db.insert_billable_metric(&m).await.unwrap();
    db.insert_charge(&charge(org, pln, m.id)).await.unwrap();
    let sub = subscription(sub_id, org, cust, pln);
    db.insert_subscription(&sub).await.unwrap();
    db.insert_wallet(&wallet(wal, org, cust, 0)).await.unwrap();
    assert!(db.set_wallet_realtime(org, wal, true, policy, Some("acct_customer")).await.unwrap());
    World { db, org, sub, wallet: wal }
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn each_event_is_held_once_at_its_price(pool: PgPool) {
    let w = world(pool, "reject").await;
    let mint = MockTransport::scripted(
        vec![ok(r#"{"success":true,"data":{"hold_id":"hold_a","status":"Open","captured_micros":0}}"#)],
        HttpResponse { status: 500, body: "{}".into() },
    );
    let e = event(w.org, &w.sub, "tx-1", 4);
    let r = reserve_for_event(&w.db, &mint, w.org, &w.sub, &e).await.unwrap();
    assert_eq!(r, Reservation::Reserved { cents: 100, mint_hold_id: "hold_a".into() });
    let sent: serde_json::Value = serde_json::from_str(&mint.requests()[0].body).unwrap();
    assert_eq!(sent["from"], "acct_customer");
    assert_eq!(sent["amount_micros"], 1_000_000);
    assert_eq!(sent["idem_key"], format!("rago:wallet:{}:event:tx-1", w.wallet));

    // A retried event is answered from the ledger without a second hold.
    let again = reserve_for_event(&w.db, &mint, w.org, &w.sub, &e).await.unwrap();
    assert_eq!(again, r);
    assert_eq!(mint.requests().len(), 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn insufficient_funds_rejects_or_flags_by_policy(pool: PgPool) {
    let w = world(pool, "reject").await;
    let broke = MockTransport::always(402, r#"{"success":false,"error":{"code":"insufficient_funds","message":"x"}}"#);
    let e = event(w.org, &w.sub, "tx-2", 8);
    assert_eq!(
        reserve_for_event(&w.db, &broke, w.org, &w.sub, &e).await.unwrap(),
        Reservation::Rejected { cents: 200 }
    );
    assert!(w.db.realtime_hold_for_event(w.wallet, "tx-2").await.unwrap().is_none());

    w.db.set_wallet_realtime(w.org, w.wallet, true, "flag", None).await.unwrap();
    let flagged = reserve_for_event(&w.db, &broke, w.org, &w.sub, &e).await.unwrap();
    assert!(matches!(flagged, Reservation::Flagged { cents: 200, .. }));
    let row = w.db.realtime_hold_for_event(w.wallet, "tx-2").await.unwrap().unwrap();
    assert_eq!(row.status, "flagged");
    assert!(row.mint_hold_id.is_none());

    // Mint unreachable under `reject` fails closed.
    w.db.set_wallet_realtime(w.org, w.wallet, true, "reject", None).await.unwrap();
    let down = MockTransport::always(503, r#"{"success":false,"error":{"code":"unavailable","message":"down"}}"#);
    let e3 = event(w.org, &w.sub, "tx-3", 1);
    assert!(matches!(
        reserve_for_event(&w.db, &down, w.org, &w.sub, &e3).await.unwrap(),
        Reservation::Unavailable { .. }
    ));
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn finalization_captures_holds_and_applies_them_to_the_invoice(pool: PgPool) {
    let w = world(pool, "reject").await;
    let holds = MockTransport::scripted(
        vec![
            ok(r#"{"success":true,"data":{"hold_id":"hold_1","status":"Open"}}"#),
            ok(r#"{"success":true,"data":{"hold_id":"hold_2","status":"Open"}}"#),
        ],
        HttpResponse { status: 500, body: "{}".into() },
    );
    for (tx, v) in [("tx-a", 4), ("tx-b", 8)] {
        let e = event(w.org, &w.sub, tx, v);
        assert!(matches!(reserve_for_event(&w.db, &holds, w.org, &w.sub, &e).await.unwrap(), Reservation::Reserved { .. }));
        assert!(w.db.insert_event(&e).await.unwrap());
    }

    // Bill the period: $10 plan fee + 12 emails x $0.25 = $13.00.
    let run = rago_jobs::bill_subscription(&w.db, w.sub.id, month_start(), month_end()).await.unwrap();
    let invoice = w.db.find_invoice(run.invoice.id).await.unwrap().unwrap();

    let mint = MockTransport::always(200, r#"{"success":true,"data":{"status":"Captured"}}"#);
    let applied = settle_invoice(&w.db, &mint, &invoice).await.unwrap();
    assert_eq!(applied, 300, "the two holds ($1 + $2) pay the usage part of the invoice");
    let captures: Vec<_> = mint.requests().into_iter().filter(|r| r.url.ends_with("/capture")).collect();
    assert_eq!(captures.len(), 2);
    let body: serde_json::Value = serde_json::from_str(&captures[0].body).unwrap();
    assert_eq!(body["to"], "acct_seller");

    let after = w.db.find_invoice(invoice.id).await.unwrap().unwrap();
    assert_eq!(after.total_amount_cents, invoice.total_amount_cents - 300);
    assert_eq!(after.prepaid_credit_amount_cents, invoice.prepaid_credit_amount_cents + 300);
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wallet_realtime_holds WHERE status = 'open'")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(open, 0);

    // Settling again is a no-op (nothing left open).
    assert_eq!(settle_invoice(&w.db, &mint, &after).await.unwrap(), 0);
}

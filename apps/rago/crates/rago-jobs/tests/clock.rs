// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Clock-job parity tests, each against an isolated migrated Postgres
//! (`#[sqlx::test]`). They prove every sweep acts only on the rows it should and
//! that the money-affecting jobs (free-trial biller, interval wallet top-ups)
//! are correct *and* idempotent.

use chrono::{Duration, NaiveDateTime, Utc};
use rago_integrations::MockTransport;
use rago_jobs::clock;
use rago_jobs::scheduler::{run_billing_cycle, SchedulerConfig};
use sqlx::{PgPool, Row};
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

async fn org(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations (id, name, slug, created_at, updated_at) VALUES ($1,'Acme',$2,now(),now())")
        .bind(id)
        .bind(format!("acme-{}", &id.to_string()[..8]))
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn customer(pool: &PgPool, org: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO customers (id, organization_id, billing_entity_id, external_id, created_at, updated_at) VALUES ($1,$2,$2,$3,now(),now())")
        .bind(id)
        .bind(org)
        .bind(format!("cust-{}", &id.to_string()[..8]))
        .execute(pool)
        .await
        .unwrap();
    id
}

/// Configure a webhook endpoint for `org` (gates webhook emission — no endpoint
/// means the clock jobs no-op the webhook, exactly like Lago materializing zero
/// `Webhook` rows for an org with no `webhook_endpoints`).
async fn webhook_endpoint(pool: &PgPool, org: Uuid) {
    rago_db::Db::new(pool.clone())
        .insert_webhook_endpoint(org, "http://hooks.test/inbound")
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn activate_subscriptions_only_activates_due(pool: PgPool) {
    let o = org(&pool).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, amount_currency, created_at, updated_at) VALUES ($1,$2,'P','p',$3,now(),now())")
        .bind(plan).bind(o).bind("USD").execute(&pool).await.unwrap();

    let due = Uuid::new_v4();
    let future = Uuid::new_v4();
    // pending (status 0), subscription_at in the past → due
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, subscription_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'due',0,$5,now(),now())")
        .bind(due).bind(o).bind(c).bind(plan).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();
    // pending, subscription_at in the future → not due
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, subscription_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'future',0,$5,now(),now())")
        .bind(future).bind(o).bind(c).bind(plan).bind(now() + Duration::hours(1)).execute(&pool).await.unwrap();

    let n = clock::subscriptions::activate_subscriptions(&rago_db::Db::new(pool.clone())).await.unwrap();
    assert_eq!(n, 1, "only the due subscription is activated");

    let due_status: i32 = sqlx::query_scalar("SELECT status FROM subscriptions WHERE id=$1").bind(due).fetch_one(&pool).await.unwrap();
    let future_status: i32 = sqlx::query_scalar("SELECT status FROM subscriptions WHERE id=$1").bind(future).fetch_one(&pool).await.unwrap();
    assert_eq!(due_status, 1);
    assert_eq!(future_status, 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn terminate_coupons_only_expired_time_limit(pool: PgPool) {
    let o = org(&pool).await;
    let expired = Uuid::new_v4();
    let future = Uuid::new_v4();
    let no_expiration = Uuid::new_v4();
    // active(0), time_limit(1), expiration_at in past → terminate
    sqlx::query("INSERT INTO coupons (id, organization_id, name, code, status, expiration, expiration_at, created_at, updated_at) VALUES ($1,$2,'e','e',0,1,$3,now(),now())")
        .bind(expired).bind(o).bind(now() - Duration::days(1)).execute(&pool).await.unwrap();
    // active, time_limit, future expiration → keep
    sqlx::query("INSERT INTO coupons (id, organization_id, name, code, status, expiration, expiration_at, created_at, updated_at) VALUES ($1,$2,'f','f',0,1,$3,now(),now())")
        .bind(future).bind(o).bind(now() + Duration::days(1)).execute(&pool).await.unwrap();
    // active, no_expiration(0) → keep even with a past date
    sqlx::query("INSERT INTO coupons (id, organization_id, name, code, status, expiration, expiration_at, created_at, updated_at) VALUES ($1,$2,'n','n',0,0,$3,now(),now())")
        .bind(no_expiration).bind(o).bind(now() - Duration::days(1)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::coupons::terminate_coupons(&db).await.unwrap();
    assert_eq!(n, 1);
    let s: i32 = sqlx::query_scalar("SELECT status FROM coupons WHERE id=$1").bind(expired).fetch_one(&pool).await.unwrap();
    assert_eq!(s, 1, "expired coupon terminated");
    let s2: i32 = sqlx::query_scalar("SELECT status FROM coupons WHERE id=$1").bind(future).fetch_one(&pool).await.unwrap();
    assert_eq!(s2, 0);
    let s3: i32 = sqlx::query_scalar("SELECT status FROM coupons WHERE id=$1").bind(no_expiration).fetch_one(&pool).await.unwrap();
    assert_eq!(s3, 0);
    // idempotent: nothing left to terminate
    assert_eq!(clock::coupons::terminate_coupons(&db).await.unwrap(), 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn mark_overdue_only_past_due_unpaid_finalized(pool: PgPool) {
    let o = org(&pool).await;
    let c = customer(&pool, o).await;
    let overdue = Uuid::new_v4();
    let not_due = Uuid::new_v4();
    let paid = Uuid::new_v4();
    // finalized(1), pending payment(0), due date in the past → overdue
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, customer_id, status, payment_status, payment_due_date, created_at, updated_at) VALUES ($1,$2,$2,$3,1,0,$4,now(),now())")
        .bind(overdue).bind(o).bind(c).bind((now() - Duration::days(2)).date()).execute(&pool).await.unwrap();
    // finalized, pending, due date in the future → not overdue
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, customer_id, status, payment_status, payment_due_date, created_at, updated_at) VALUES ($1,$2,$2,$3,1,0,$4,now(),now())")
        .bind(not_due).bind(o).bind(c).bind((now() + Duration::days(5)).date()).execute(&pool).await.unwrap();
    // finalized, succeeded(1), past due → not overdue (already paid)
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, customer_id, status, payment_status, payment_due_date, created_at, updated_at) VALUES ($1,$2,$2,$3,1,1,$4,now(),now())")
        .bind(paid).bind(o).bind(c).bind((now() - Duration::days(2)).date()).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::invoices::mark_invoices_as_payment_overdue(&db).await.unwrap();
    assert_eq!(n, 1);
    let f: bool = sqlx::query_scalar("SELECT payment_overdue FROM invoices WHERE id=$1").bind(overdue).fetch_one(&pool).await.unwrap();
    assert!(f);
    let g: bool = sqlx::query_scalar("SELECT payment_overdue FROM invoices WHERE id=$1").bind(not_due).fetch_one(&pool).await.unwrap();
    assert!(!g);
    let h: bool = sqlx::query_scalar("SELECT payment_overdue FROM invoices WHERE id=$1").bind(paid).fetch_one(&pool).await.unwrap();
    assert!(!h);
    // idempotent
    assert_eq!(clock::invoices::mark_invoices_as_payment_overdue(&db).await.unwrap(), 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn finalize_invoices_only_ready_drafts(pool: PgPool) {
    let o = org(&pool).await;
    let ready = Uuid::new_v4();
    let future = Uuid::new_v4();
    // draft(0), issuing_date today → finalize
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, status, issuing_date, created_at, updated_at) VALUES ($1,$2,$2,0,CURRENT_DATE,now(),now())")
        .bind(ready).bind(o).execute(&pool).await.unwrap();
    // draft, issuing_date in future → keep draft
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, status, issuing_date, created_at, updated_at) VALUES ($1,$2,$2,0,CURRENT_DATE + 10,now(),now())")
        .bind(future).bind(o).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::invoices::finalize_invoices(&db).await.unwrap();
    assert_eq!(n, 1);
    let s: i32 = sqlx::query_scalar("SELECT status FROM invoices WHERE id=$1").bind(ready).fetch_one(&pool).await.unwrap();
    assert_eq!(s, 1);
    let s2: i32 = sqlx::query_scalar("SELECT status FROM invoices WHERE id=$1").bind(future).fetch_one(&pool).await.unwrap();
    assert_eq!(s2, 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn refresh_draft_invoices_recomputes_totals(pool: PgPool) {
    let o = org(&pool).await;
    let inv = Uuid::new_v4();
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, status, created_at, updated_at) VALUES ($1,$2,$2,0,now(),now())")
        .bind(inv).bind(o).execute(&pool).await.unwrap();
    // two fees: 700 + 300 amount, 70 + 30 taxes
    for (amt, tax) in [(700i64, 70i64), (300i64, 30i64)] {
        sqlx::query("INSERT INTO fees (id, organization_id, billing_entity_id, invoice_id, fee_type, amount_cents, amount_currency, taxes_amount_cents, created_at, updated_at) VALUES (gen_random_uuid(),$1,$1,$2,0,$3,'USD',$4,now(),now())")
            .bind(o).bind(inv).bind(amt).bind(tax).execute(&pool).await.unwrap();
    }

    let db = rago_db::Db::new(pool.clone());
    let n = clock::invoices::refresh_draft_invoices(&db).await.unwrap();
    assert_eq!(n, 1);
    let row = sqlx::query("SELECT fees_amount_cents, taxes_amount_cents, total_amount_cents FROM invoices WHERE id=$1").bind(inv).fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<i64, _>("fees_amount_cents"), 1000);
    assert_eq!(row.get::<i64, _>("taxes_amount_cents"), 100);
    assert_eq!(row.get::<i64, _>("total_amount_cents"), 1100);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn terminate_wallets_only_expired(pool: PgPool) {
    let o = org(&pool).await;
    let c = customer(&pool, o).await;
    let expired = Uuid::new_v4();
    let active = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, expiration_at, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',$4,now(),now())")
        .bind(expired).bind(o).bind(c).bind(now() - Duration::days(1)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, expiration_at, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',$4,now(),now())")
        .bind(active).bind(o).bind(c).bind(now() + Duration::days(30)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::wallets::terminate_wallets(&db).await.unwrap();
    assert_eq!(n, 1);
    let s: i32 = sqlx::query_scalar("SELECT status FROM wallets WHERE id=$1").bind(expired).fetch_one(&pool).await.unwrap();
    assert_eq!(s, 1);
    let s2: i32 = sqlx::query_scalar("SELECT status FROM wallets WHERE id=$1").bind(active).fetch_one(&pool).await.unwrap();
    assert_eq!(s2, 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn interval_wallet_topup_is_correct_and_idempotent(pool: PgPool) {
    let o = org(&pool).await;
    let c = customer(&pool, o).await;
    let wallet = Uuid::new_v4();
    // rate 1.0 credit = 1 currency unit; empty balance
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, rate_amount, balance_cents, credits_balance, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',1.0,0,0,now(),now())")
        .bind(wallet).bind(o).bind(c).execute(&pool).await.unwrap();
    // due interval rule: 10 paid + 5 granted = 15 credits, due an hour ago
    let due_rule = Uuid::new_v4();
    sqlx::query("INSERT INTO wallet_recurring_rules (id, organization_id, wallet_id, rule_type, interval, paid_credits, granted_credits, status, next_top_up_at, created_at, updated_at) VALUES ($1,$2,$3,'interval','monthly',10,5,0,$4,now(),now())")
        .bind(due_rule).bind(o).bind(wallet).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();
    // not-due rule on a second wallet
    let wallet2 = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, rate_amount, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',1.0,now(),now())")
        .bind(wallet2).bind(o).bind(c).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wallet_recurring_rules (id, organization_id, wallet_id, rule_type, interval, paid_credits, granted_credits, status, next_top_up_at, created_at, updated_at) VALUES (gen_random_uuid(),$1,$2,'interval','monthly',20,0,0,$3,now(),now())")
        .bind(o).bind(wallet2).bind(now() + Duration::days(10)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::wallets::create_interval_wallet_transactions(&db).await.unwrap();
    assert_eq!(n, 1, "only the due rule tops up");

    let credits: rust_decimal::Decimal = sqlx::query_scalar("SELECT credits_balance FROM wallets WHERE id=$1").bind(wallet).fetch_one(&pool).await.unwrap();
    assert_eq!(credits.to_string(), "15.00000");
    let cents: i64 = sqlx::query_scalar("SELECT balance_cents FROM wallets WHERE id=$1").bind(wallet).fetch_one(&pool).await.unwrap();
    assert_eq!(cents, 1500);
    let tx: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wallet_transactions WHERE wallet_id=$1").bind(wallet).fetch_one(&pool).await.unwrap();
    assert_eq!(tx, 1);

    // idempotent: rule now scheduled a month out, so a re-run tops up nothing.
    let n2 = clock::wallets::create_interval_wallet_transactions(&db).await.unwrap();
    assert_eq!(n2, 0, "no double top-up in the same interval");
    let cents2: i64 = sqlx::query_scalar("SELECT balance_cents FROM wallets WHERE id=$1").bind(wallet).fetch_one(&pool).await.unwrap();
    assert_eq!(cents2, 1500);
    let tx2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wallet_transactions WHERE wallet_id=$1").bind(wallet).fetch_one(&pool).await.unwrap();
    assert_eq!(tx2, 1);

    // untouched wallet
    let cents3: i64 = sqlx::query_scalar("SELECT balance_cents FROM wallets WHERE id=$1").bind(wallet2).fetch_one(&pool).await.unwrap();
    assert_eq!(cents3, 0);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn free_trial_biller_bills_elapsed_trials_once(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    // pay-in-advance monthly plan, 7-day trial, 1000 cents
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, interval, amount_cents, amount_currency, trial_period, pay_in_advance, created_at, updated_at) VALUES ($1,$2,'Pro','pro',1,1000,'USD',7,true,now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();

    let elapsed = Uuid::new_v4();
    let ongoing = Uuid::new_v4();
    // active, started 10 days ago → trial (7d) elapsed
    let started = now() - Duration::days(10);
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, billing_time, subscription_at, started_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'elapsed',1,0,$5,$5,now(),now())")
        .bind(elapsed).bind(o).bind(c).bind(plan).bind(started).execute(&pool).await.unwrap();
    // active, started 1 day ago → trial not elapsed
    let started2 = now() - Duration::days(1);
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, billing_time, subscription_at, started_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'ongoing',1,0,$5,$5,now(),now())")
        .bind(ongoing).bind(o).bind(c).bind(plan).bind(started2).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::subscriptions::free_trial_biller(&db).await.unwrap();
    assert_eq!(n, 1, "only the elapsed-trial subscription is billed");

    let ended: Option<NaiveDateTime> = sqlx::query_scalar("SELECT trial_ended_at FROM subscriptions WHERE id=$1").bind(elapsed).fetch_one(&pool).await.unwrap();
    assert!(ended.is_some(), "trial closed");
    let ongoing_ended: Option<NaiveDateTime> = sqlx::query_scalar("SELECT trial_ended_at FROM subscriptions WHERE id=$1").bind(ongoing).fetch_one(&pool).await.unwrap();
    assert!(ongoing_ended.is_none());

    let invoices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id=$1").bind(c).fetch_one(&pool).await.unwrap();
    assert_eq!(invoices, 1, "one first-period invoice for the pay-in-advance trial");
    let alerts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhooks WHERE webhook_type='subscription.trial_ended'").fetch_one(&pool).await.unwrap();
    assert_eq!(alerts, 1);

    // idempotent: trial_ended_at gates re-selection → no new invoice.
    let n2 = clock::subscriptions::free_trial_biller(&db).await.unwrap();
    assert_eq!(n2, 0);
    let invoices2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invoices WHERE customer_id=$1").bind(c).fetch_one(&pool).await.unwrap();
    assert_eq!(invoices2, 1, "no duplicate trial invoice on re-run");
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn termination_alert_emits_once_per_day(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, amount_currency, created_at, updated_at) VALUES ($1,$2,'P','p','USD',now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();
    // active, ending in exactly 15 days → alert window
    let sub = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, ending_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'ends',1, (CURRENT_DATE + 15)::timestamp + INTERVAL '12 hours', now(),now())")
        .bind(sub).bind(o).bind(c).bind(plan).execute(&pool).await.unwrap();
    // active, ending in 3 days → outside the 15/45 windows
    let sub2 = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, ending_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'soon',1, (CURRENT_DATE + 3)::timestamp, now(),now())")
        .bind(sub2).bind(o).bind(c).bind(plan).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::subscriptions::subscriptions_to_be_terminated(&db).await.unwrap();
    assert_eq!(n, 1);
    // re-run same day → dedup, no second alert
    let n2 = clock::subscriptions::subscriptions_to_be_terminated(&db).await.unwrap();
    assert_eq!(n2, 0);
    let alerts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhooks WHERE webhook_type='subscription.termination_alert'").fetch_one(&pool).await.unwrap();
    assert_eq!(alerts, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn compute_daily_usages_snapshots_today(pool: PgPool) {
    let o = org(&pool).await;
    // two events today for one subscription, one yesterday
    sqlx::query("INSERT INTO events (id, organization_id, transaction_id, code, external_subscription_id, timestamp, precise_total_amount_cents, created_at, updated_at) VALUES (gen_random_uuid(),$1,'t1','api','sub-1', now(), 250, now(), now())")
        .bind(o).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO events (id, organization_id, transaction_id, code, external_subscription_id, timestamp, precise_total_amount_cents, created_at, updated_at) VALUES (gen_random_uuid(),$1,'t2','api','sub-1', now(), 750, now(), now())")
        .bind(o).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO events (id, organization_id, transaction_id, code, external_subscription_id, timestamp, precise_total_amount_cents, created_at, updated_at) VALUES (gen_random_uuid(),$1,'t3','api','sub-1', now() - INTERVAL '1 day', 999, now(), now())")
        .bind(o).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::usage::compute_all_daily_usages(&db).await.unwrap();
    assert_eq!(n, 1);
    let amt: i64 = sqlx::query_scalar("SELECT amount_cents FROM daily_usages WHERE subscription_external_id='sub-1' AND usage_date = CURRENT_DATE").fetch_one(&pool).await.unwrap();
    assert_eq!(amt, 1000, "today's usage summed; yesterday excluded");
    // idempotent upsert
    let n2 = clock::usage::compute_all_daily_usages(&db).await.unwrap();
    assert_eq!(n2, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM daily_usages WHERE subscription_external_id='sub-1'").fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn webhooks_cleanup_deletes_only_old(pool: PgPool) {
    let o = org(&pool).await;
    // old (100 days) and recent (1 day) delivery records
    sqlx::query("INSERT INTO webhooks (id, organization_id, webhook_type, object_type, endpoint, status, created_at, updated_at) VALUES (gen_random_uuid(),$1,'invoice.created','invoice','http://x','succeeded', now() - INTERVAL '100 days', now() - INTERVAL '100 days')")
        .bind(o).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO webhooks (id, organization_id, webhook_type, object_type, endpoint, status, created_at, updated_at) VALUES (gen_random_uuid(),$1,'invoice.created','invoice','http://x','succeeded', now() - INTERVAL '1 day', now() - INTERVAL '1 day')")
        .bind(o).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::webhooks::webhooks_cleanup(&db).await.unwrap();
    assert_eq!(n, 1);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhooks").fetch_one(&pool).await.unwrap();
    assert_eq!(left, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn inbound_webhooks_retry_and_cleanup(pool: PgPool) {
    let o = org(&pool).await;
    // pending → retriable; old succeeded → cleaned up
    sqlx::query("INSERT INTO inbound_webhooks (id, organization_id, source, status, processing_attempts, created_at, updated_at) VALUES (gen_random_uuid(),$1,'stripe','pending',0, now(), now())")
        .bind(o).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO inbound_webhooks (id, organization_id, source, status, processing_attempts, created_at, updated_at) VALUES (gen_random_uuid(),$1,'stripe','succeeded',1, now() - INTERVAL '100 days', now() - INTERVAL '100 days')")
        .bind(o).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let retried = clock::webhooks::inbound_webhooks_retry(&db).await.unwrap();
    assert_eq!(retried, 1);
    let processing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_webhooks WHERE status='processing' AND processing_attempts=1").fetch_one(&pool).await.unwrap();
    assert_eq!(processing, 1);

    let cleaned = clock::webhooks::inbound_webhooks_cleanup(&db).await.unwrap();
    assert_eq!(cleaned, 1);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_webhooks").fetch_one(&pool).await.unwrap();
    assert_eq!(left, 1);
}

/// Count webhooks of a given type for verification.
async fn webhook_count(pool: &PgPool, webhook_type: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM webhooks WHERE webhook_type = $1")
        .bind(webhook_type)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn activate_emits_subscription_started_when_endpoint(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, amount_currency, created_at, updated_at) VALUES ($1,$2,'P','p','USD',now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();
    let due = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, subscription_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'due',0,$5,now(),now())")
        .bind(due).bind(o).bind(c).bind(plan).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::subscriptions::activate_subscriptions(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "subscription.started").await, 1);
    // Envelope shape: object nested under `subscription`, carrying lago_id.
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM webhooks WHERE webhook_type='subscription.started'").fetch_one(&pool).await.unwrap();
    assert_eq!(payload["webhook_type"], "subscription.started");
    assert_eq!(payload["subscription"]["lago_id"], due.to_string());

    // Re-run: subscription no longer pending → no duplicate webhook.
    let n2 = clock::subscriptions::activate_subscriptions(&db).await.unwrap();
    assert_eq!(n2, 0);
    assert_eq!(webhook_count(&pool, "subscription.started").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn activate_no_endpoint_is_a_noop(pool: PgPool) {
    // No webhook_endpoint configured → no webhook row (Lago no-ops), but the
    // subscription is still activated.
    let o = org(&pool).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, amount_currency, created_at, updated_at) VALUES ($1,$2,'P','p','USD',now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();
    let due = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, subscription_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'due',0,$5,now(),now())")
        .bind(due).bind(o).bind(c).bind(plan).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::subscriptions::activate_subscriptions(&db).await.unwrap();
    assert_eq!(n, 1, "activation still happens");
    let status: i32 = sqlx::query_scalar("SELECT status FROM subscriptions WHERE id=$1").bind(due).fetch_one(&pool).await.unwrap();
    assert_eq!(status, 1);
    assert_eq!(webhook_count(&pool, "subscription.started").await, 0, "no endpoint -> no webhook");
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn terminate_ended_emits_subscription_terminated(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, amount_currency, created_at, updated_at) VALUES ($1,$2,'P','p','USD',now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();
    // active, ended in the past → terminate
    let ended = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, ending_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'ended',1,$5,now(),now())")
        .bind(ended).bind(o).bind(c).bind(plan).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();
    // active, ends later → keep
    let live = Uuid::new_v4();
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, ending_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'live',1,$5,now(),now())")
        .bind(live).bind(o).bind(c).bind(plan).bind(now() + Duration::days(10)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::subscriptions::terminate_ended_subscriptions(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "subscription.terminated").await, 1);
    // idempotent
    assert_eq!(clock::subscriptions::terminate_ended_subscriptions(&db).await.unwrap(), 0);
    assert_eq!(webhook_count(&pool, "subscription.terminated").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn finalize_emits_invoice_created(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let ready = Uuid::new_v4();
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, status, issuing_date, created_at, updated_at) VALUES ($1,$2,$2,0,CURRENT_DATE,now(),now())")
        .bind(ready).bind(o).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::invoices::finalize_invoices(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "invoice.created").await, 1);
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM webhooks WHERE webhook_type='invoice.created'").fetch_one(&pool).await.unwrap();
    assert_eq!(payload["invoice"]["lago_id"], ready.to_string());
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn mark_overdue_emits_invoice_payment_overdue(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let overdue = Uuid::new_v4();
    sqlx::query("INSERT INTO invoices (id, organization_id, billing_entity_id, customer_id, status, payment_status, payment_due_date, created_at, updated_at) VALUES ($1,$2,$2,$3,1,0,$4,now(),now())")
        .bind(overdue).bind(o).bind(c).bind((now() - Duration::days(2)).date()).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::invoices::mark_invoices_as_payment_overdue(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "invoice.payment_overdue").await, 1);
    // idempotent: already flagged → no re-emit.
    assert_eq!(clock::invoices::mark_invoices_as_payment_overdue(&db).await.unwrap(), 0);
    assert_eq!(webhook_count(&pool, "invoice.payment_overdue").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn terminate_wallets_emits_wallet_terminated(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let expired = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, expiration_at, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',$4,now(),now())")
        .bind(expired).bind(o).bind(c).bind(now() - Duration::days(1)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::wallets::terminate_wallets(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "wallet.terminated").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn refresh_ongoing_emits_depleted_only_on_transition(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    // Was positive (100), usage 150 > balance 100 → new ongoing -50 → transition.
    let depleting = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, balance_cents, ongoing_usage_balance_cents, ongoing_balance_cents, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',100,150,100,now(),now())")
        .bind(depleting).bind(o).bind(c).execute(&pool).await.unwrap();
    // Healthy: stays positive → no webhook.
    let healthy = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, balance_cents, ongoing_usage_balance_cents, ongoing_balance_cents, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',1000,0,1000,now(),now())")
        .bind(healthy).bind(o).bind(c).execute(&pool).await.unwrap();
    // Already depleted (old 0, not > 0) → no re-alert.
    let already = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, balance_cents, ongoing_usage_balance_cents, ongoing_balance_cents, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',0,50,0,now(),now())")
        .bind(already).bind(o).bind(c).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::wallets::refresh_wallets_ongoing_balance(&db).await.unwrap();
    assert_eq!(n, 3, "all active wallets refreshed");
    assert_eq!(webhook_count(&pool, "wallet.depleted_ongoing_balance").await, 1, "only the transitioning wallet alerts");
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM webhooks WHERE webhook_type='wallet.depleted_ongoing_balance'").fetch_one(&pool).await.unwrap();
    assert_eq!(payload["wallet"]["lago_id"], depleting.to_string());

    // Re-run: the depleting wallet is now already <= 0 (old not > 0) → no re-alert.
    let n2 = clock::wallets::refresh_wallets_ongoing_balance(&db).await.unwrap();
    assert_eq!(n2, 3);
    assert_eq!(webhook_count(&pool, "wallet.depleted_ongoing_balance").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn interval_topup_emits_wallet_transaction_created(pool: PgPool) {
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let wallet = Uuid::new_v4();
    sqlx::query("INSERT INTO wallets (id, organization_id, customer_id, status, balance_currency, rate_amount, balance_cents, credits_balance, created_at, updated_at) VALUES ($1,$2,$3,0,'USD',1.0,0,0,now(),now())")
        .bind(wallet).bind(o).bind(c).execute(&pool).await.unwrap();
    let rule = Uuid::new_v4();
    sqlx::query("INSERT INTO wallet_recurring_rules (id, organization_id, wallet_id, rule_type, interval, paid_credits, granted_credits, status, next_top_up_at, created_at, updated_at) VALUES ($1,$2,$3,'interval','monthly',10,5,0,$4,now(),now())")
        .bind(rule).bind(o).bind(wallet).bind(now() - Duration::hours(1)).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let n = clock::wallets::create_interval_wallet_transactions(&db).await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(webhook_count(&pool, "wallet_transaction.created").await, 1);
    // idempotent: rescheduled a month out → no second top-up, no second webhook.
    let n2 = clock::wallets::create_interval_wallet_transactions(&db).await.unwrap();
    assert_eq!(n2, 0);
    assert_eq!(webhook_count(&pool, "wallet_transaction.created").await, 1);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn billing_cycle_emits_invoice_created_and_fee_created(pool: PgPool) {
    // The delivered (scheduler) path emits `invoice.created` then one
    // `fee.created` per assembled fee, mirroring `Invoices::SubscriptionService`.
    let o = org(&pool).await;
    webhook_endpoint(&pool, o).await;
    let c = customer(&pool, o).await;
    let plan = Uuid::new_v4();
    // arrears monthly plan with a flat subscription fee, no usage charges → the
    // generation yields exactly one (subscription) fee.
    sqlx::query("INSERT INTO plans (id, organization_id, name, code, interval, amount_cents, amount_currency, trial_period, pay_in_advance, created_at, updated_at) VALUES ($1,$2,'Flat','flat',1,1000,'USD',0,false,now(),now())")
        .bind(plan).bind(o).execute(&pool).await.unwrap();
    let sub = Uuid::new_v4();
    let started = now() - Duration::days(70);
    sqlx::query("INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, external_id, status, billing_time, subscription_at, started_at, created_at, updated_at) VALUES ($1,$2,$3,$4,'flat-sub',1,0,$5,$5,now(),now())")
        .bind(sub).bind(o).bind(c).bind(plan).bind(started).execute(&pool).await.unwrap();

    let db = rago_db::Db::new(pool.clone());
    let transport = MockTransport::always(200, "{}");
    let cfg = SchedulerConfig { charge_customers: false };
    let report = run_billing_cycle(&db, &transport, &cfg, Utc::now().date_naive(), true)
        .await
        .unwrap();
    assert_eq!(report.billed_invoices.len(), 1);
    assert_eq!(webhook_count(&pool, "invoice.created").await, 1);
    assert_eq!(webhook_count(&pool, "fee.created").await, 1, "one fee.created per assembled fee");
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM webhooks WHERE webhook_type='fee.created'").fetch_one(&pool).await.unwrap();
    assert_eq!(payload["object_type"], "fee");
    assert_eq!(payload["fee"]["amount_cents"], 1000);
}

#[sqlx::test(migrations = "../rago-db/migrations")]
async fn run_all_executes_every_registered_job(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let report = clock::run_all(&db).await.unwrap();
    assert_eq!(report.ran.len(), clock::ALL_JOBS.len());
    assert_eq!(report.total_affected(), 0, "empty DB → no rows transitioned");
}

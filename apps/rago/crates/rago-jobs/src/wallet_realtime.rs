//! Real-time prepaid wallet debit through Mint holds (orchestration).
//!
//! * [`reserve_for_event`]: called by `POST /events` BEFORE the event is
//!   stored. It prices the event at its marginal cost with the plan's charge
//!   models and reserves that amount on the customer's Mint account. The Mint
//!   idempotency key is derived from the event `transaction_id`, so a retried
//!   event never holds twice. When the customer cannot cover it, the wallet's
//!   policy decides: `reject` (the API answers 422 and nothing is stored) or
//!   `flag` (the event is stored and the shortfall recorded).
//! * [`settle_invoice`]: called when an invoice is finalized. It captures the
//!   invoice's open holds to the seller's Mint account, up to the amount due,
//!   releases the rest, and applies the captured amount to the invoice as
//!   prepaid credit. A fully covered invoice is paid with no provider charge.
//! * [`release_invoice_holds`]: called on void. It releases holds that are
//!   still reserved for the voided invoice.
//!
//! Mint connection: `RAGO_MINT_URL`, `RAGO_MINT_API_KEY` and
//! `RAGO_MINT_PAYEE_ACCOUNT` (the seller's Mint account that captures are paid
//! to).

use chrono::{Datelike, NaiveDate, Utc};
use rago_billing::dates::MonthlyDates;
use rago_billing::wallet_realtime::{
    capture_idem_key, cents_to_micros, hold_idem_key, incremental_event_cost, micros_to_cents,
    plan_settlement, release_idem_key, RealtimePolicy, SettlementStep,
};
use rago_core::enums::BillingTime;
use rago_core::models::{Event, Invoice, Subscription};
use rago_db::{Db, RealtimeHold, RealtimeWallet};
use rago_integrations::{HttpTransport, MintError, MintHoldsClient};
use uuid::Uuid;

use crate::JobError;

/// The Mint server real-time wallets settle on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintConfig {
    pub url: String,
    pub api_key: String,
    pub payee_account: String,
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// `RAGO_MINT_URL` + `RAGO_MINT_API_KEY` + `RAGO_MINT_PAYEE_ACCOUNT`, when all
/// are set.
pub fn mint_config() -> Option<MintConfig> {
    Some(MintConfig {
        url: env_nonempty("RAGO_MINT_URL")?,
        api_key: env_nonempty("RAGO_MINT_API_KEY")?,
        payee_account: env_nonempty("RAGO_MINT_PAYEE_ACCOUNT")?,
    })
}

/// What happened to an incoming event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// The customer has no real-time wallet or the event is not billable.
    NotApplicable,
    /// Billable but priced at zero (free units, inside a package...).
    Free,
    /// The price is held at Mint.
    Reserved { cents: i64, mint_hold_id: String },
    /// Stored anyway under the `flag` policy; the shortfall is recorded.
    Flagged { cents: i64, reason: String },
    /// Refused under the `reject` policy (insufficient Mint balance).
    Rejected { cents: i64 },
    /// Refused under the `reject` policy because Mint could not be reached or
    /// the wallet cannot settle through Mint (fail closed).
    Unavailable { reason: String },
}

/// Current billing period `[from, to]` of a subscription (calendar or
/// anniversary), the same window `current_usage` prices.
fn current_period(sub: &Subscription, pay_in_advance: bool) -> (NaiveDate, NaiveDate) {
    let today = Utc::now().date_naive();
    let billing_at = if today.month() == 12 {
        NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
    }
    .unwrap_or(today);
    let dates = MonthlyDates {
        subscription_at: sub.subscription_at.map(|t| t.date_naive()).unwrap_or(today),
        billing_at,
        anniversary: matches!(sub.billing_time, BillingTime::Anniversary),
        pay_in_advance,
        current_usage: false,
    };
    let b = dates.boundaries();
    (b.from_date, b.to_date)
}

/// Price `event` and reserve it on the customer's real-time wallet (see the
/// module docs). `event` is the not-yet-stored event.
pub async fn reserve_for_event<T: HttpTransport>(
    db: &Db,
    transport: &T,
    organization_id: Uuid,
    sub: &Subscription,
    event: &Event,
) -> Result<Reservation, JobError> {
    let Some(wallet) = db.realtime_wallet_for_customer(sub.customer_id).await? else {
        return Ok(Reservation::NotApplicable);
    };
    if wallet.organization_id != organization_id {
        return Ok(Reservation::NotApplicable);
    }
    if let Some(existing) = db.realtime_hold_for_event(wallet.wallet_id, &event.transaction_id).await? {
        return Ok(match (existing.status.as_str(), existing.mint_hold_id) {
            ("flagged", _) => Reservation::Flagged { cents: existing.amount_cents, reason: "replay".into() },
            (_, Some(id)) => Reservation::Reserved { cents: existing.amount_cents, mint_hold_id: id },
            _ => Reservation::Free,
        });
    }
    let policy = RealtimePolicy::parse(&wallet.policy).unwrap_or(RealtimePolicy::Reject);
    let Some(metric) = db.find_metric_by_code(organization_id, &event.code).await? else {
        return Ok(Reservation::NotApplicable);
    };
    let plan = db.find_plan(sub.plan_id).await?;
    let charges: Vec<_> = db
        .charges_for_plan(plan.id)
        .await?
        .into_iter()
        .filter(|c| c.billable_metric_id == Some(metric.id) && c.deleted_at.is_none())
        .collect();
    if charges.is_empty() {
        return Ok(Reservation::NotApplicable);
    }
    let (from, to) = current_period(sub, plan.pay_in_advance);
    let period_events = db
        .events_for_external_subscription(
            organization_id,
            &sub.external_id,
            &metric.code,
            from.and_hms_opt(0, 0, 0).unwrap_or_default(),
            to.and_hms_opt(23, 59, 59).unwrap_or_default(),
        )
        .await?;
    let cents = incremental_event_cost(&metric, &charges, sub, &period_events, event, plan.currency())
        .map_err(|e| JobError::Payment(format!("event pricing failed: {e}")))?;
    if cents <= 0 {
        return Ok(Reservation::Free);
    }
    let micros = match cents_to_micros(cents, &wallet.currency) {
        Ok(m) => m,
        Err(e) => return fail(db, &wallet, sub, event, cents, 0, policy, e.to_string()).await,
    };
    let Some(config) = mint_config() else {
        return fail(db, &wallet, sub, event, cents, micros, policy, "mint_not_configured".into()).await;
    };
    let client = MintHoldsClient::new(config.url, config.api_key, transport);
    let reference = format!("rago:event:{}", event.transaction_id);
    match client
        .create_hold(&wallet.mint_account_id, micros, &hold_idem_key(wallet.wallet_id, &event.transaction_id), &reference)
        .await
    {
        Ok(hold) => {
            let row = hold_row(&wallet, sub, event, Some(hold.hold_id.clone()), cents, micros, "open");
            db.insert_realtime_hold(&row, None).await?;
            Ok(Reservation::Reserved { cents, mint_hold_id: hold.hold_id })
        }
        Err(MintError::InsufficientFunds) => match policy {
            RealtimePolicy::Reject => Ok(Reservation::Rejected { cents }),
            RealtimePolicy::Flag => {
                let row = hold_row(&wallet, sub, event, None, cents, micros, "flagged");
                db.insert_realtime_hold(&row, Some("insufficient_funds")).await?;
                Ok(Reservation::Flagged { cents, reason: "insufficient_funds".into() })
            }
        },
        Err(e) => fail(db, &wallet, sub, event, cents, micros, policy, e.to_string()).await,
    }
}

/// Mint unusable for this event: fail closed (`reject`) or record it (`flag`).
#[allow(clippy::too_many_arguments)]
async fn fail(
    db: &Db,
    wallet: &RealtimeWallet,
    sub: &Subscription,
    event: &Event,
    cents: i64,
    micros: i64,
    policy: RealtimePolicy,
    reason: String,
) -> Result<Reservation, JobError> {
    tracing::warn!(wallet_id = %wallet.wallet_id, reason = %reason, "real-time wallet could not reserve an event");
    match policy {
        RealtimePolicy::Reject => Ok(Reservation::Unavailable { reason }),
        RealtimePolicy::Flag => {
            let row = hold_row(wallet, sub, event, None, cents, micros, "flagged");
            db.insert_realtime_hold(&row, Some(&reason)).await?;
            Ok(Reservation::Flagged { cents, reason })
        }
    }
}

fn hold_row(
    wallet: &RealtimeWallet,
    sub: &Subscription,
    event: &Event,
    mint_hold_id: Option<String>,
    cents: i64,
    micros: i64,
    status: &str,
) -> RealtimeHold {
    RealtimeHold {
        id: Uuid::new_v4(),
        organization_id: wallet.organization_id,
        wallet_id: wallet.wallet_id,
        customer_id: wallet.customer_id,
        subscription_id: sub.id,
        event_transaction_id: event.transaction_id.clone(),
        event_timestamp: event.timestamp.unwrap_or_else(|| Utc::now().naive_utc()),
        mint_hold_id,
        amount_cents: cents,
        amount_micros: micros,
        status: status.to_string(),
        invoice_id: None,
        captured_micros: 0,
    }
}

/// Release the hold of an event that was reserved but then not stored (a
/// concurrent duplicate won the insert race).
pub async fn release_event_hold<T: HttpTransport>(
    db: &Db,
    transport: &T,
    customer_id: Uuid,
    transaction_id: &str,
) -> Result<(), JobError> {
    let Some(wallet) = db.realtime_wallet_for_customer(customer_id).await? else {
        return Ok(());
    };
    let Some(hold) = db.realtime_hold_for_event(wallet.wallet_id, transaction_id).await? else {
        return Ok(());
    };
    release(db, transport, &hold, "duplicate").await
}

async fn release<T: HttpTransport>(db: &Db, transport: &T, hold: &RealtimeHold, reason: &str) -> Result<(), JobError> {
    let (Some(config), Some(mint_hold)) = (mint_config(), hold.mint_hold_id.as_deref()) else {
        return Ok(());
    };
    if hold.status != "open" {
        return Ok(());
    }
    let client = MintHoldsClient::new(config.url, config.api_key, transport);
    match client.release(mint_hold, &release_idem_key(mint_hold, reason), reason).await {
        // A hold Mint already closed (expired / released) is released for us too.
        Ok(_) | Err(MintError::NotFound(_)) => {
            db.settle_realtime_hold(hold.id, "released", None, 0, None).await?;
            Ok(())
        }
        Err(e) => {
            db.settle_realtime_hold(hold.id, "open", None, 0, Some(&e.to_string())).await?;
            Err(JobError::Payment(format!("mint release failed: {e}")))
        }
    }
}

/// Capture the invoice's reserved events (see the module docs). Returns the
/// cents applied to the invoice.
pub async fn settle_invoice<T: HttpTransport>(db: &Db, transport: &T, invoice: &Invoice) -> Result<i64, JobError> {
    let holds = db.open_realtime_holds_for_invoice(invoice.id).await?;
    let holds: Vec<RealtimeHold> = holds.into_iter().filter(|h| h.mint_hold_id.is_some()).collect();
    if holds.is_empty() {
        return Ok(0);
    }
    let Some(config) = mint_config() else {
        tracing::warn!(invoice_id = %invoice.id, "real-time holds found but Mint is not configured; left open");
        return Ok(0);
    };
    let currency = invoice.currency().code();
    let due_micros = cents_to_micros(invoice.total_amount_cents.max(0), &currency).unwrap_or(0);
    db.bind_realtime_holds_to_invoice(&holds.iter().map(|h| h.id).collect::<Vec<_>>(), invoice.id)
        .await?;
    let plan_input: Vec<(String, i64)> = holds
        .iter()
        .map(|h| (h.mint_hold_id.clone().unwrap_or_default(), h.amount_micros))
        .collect();
    let client = MintHoldsClient::new(config.url.clone(), config.api_key.clone(), transport);
    let mut captured_micros = 0i64;
    for (step, hold) in plan_settlement(&plan_input, due_micros).into_iter().zip(holds.iter()) {
        match step {
            SettlementStep::Capture { hold_id, micros } => {
                match client
                    .capture(&hold_id, micros, &config.payee_account, &capture_idem_key(&hold_id, invoice.id), &invoice.number)
                    .await
                {
                    Ok(result) => {
                        let got = if result.captured_micros > 0 { result.captured_micros } else { micros };
                        captured_micros += got;
                        db.settle_realtime_hold(hold.id, "captured", Some(invoice.id), got, None).await?;
                    }
                    Err(e) => {
                        tracing::error!(invoice_id = %invoice.id, hold_id = %hold_id, error = %e, "mint capture failed; hold stays reserved");
                        db.settle_realtime_hold(hold.id, "open", Some(invoice.id), 0, Some(&e.to_string())).await?;
                    }
                }
            }
            SettlementStep::Release { .. } => {
                if let Err(e) = release(db, transport, hold, "surplus").await {
                    tracing::warn!(invoice_id = %invoice.id, error = %e, "surplus hold release failed");
                }
            }
        }
    }
    let applied = db
        .apply_realtime_capture_to_invoice(invoice.id, micros_to_cents(captured_micros))
        .await?;
    Ok(applied)
}

/// Release every hold still reserved for a voided invoice. Returns how many
/// were released.
pub async fn release_invoice_holds<T: HttpTransport>(db: &Db, transport: &T, invoice_id: Uuid) -> Result<usize, JobError> {
    let holds = db.open_realtime_holds_bound_to_invoice(invoice_id).await?;
    let mut released = 0;
    for hold in &holds {
        if release(db, transport, hold, "void").await.is_ok() {
            released += 1;
        }
    }
    Ok(released)
}

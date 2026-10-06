//! `Invoices::RefreshDraftService` — re-price a draft invoice's usage.
//!
//! A grace period keeps a periodic invoice as a draft so late events of the
//! closed period still count. Before this, a refresh only re-summed the fees
//! already on the draft, so the late events were never billed. Now the usage
//! lines are priced again from the events of the window recorded on them
//! (`charges_from_datetime` / `charges_to_datetime` on each usage fee), with
//! the plan's current charges, filters and groups; the other lines (the
//! subscription fee, fixed charges) stay; taxes and totals are recomputed.
//!
//! A draft whose totals carry progressive-billing credits or coupons, or whose
//! usage lines predate the recorded window, keeps the old behaviour (totals
//! re-summed from its lines): re-pricing those exactly needs state the draft
//! does not hold.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDateTime};
use rago_billing::generate::charge_lines;
use rago_billing::invoices::assemble_invoice;
use rago_billing::proration::{DayRange, InvoicePeriod};
use rago_core::enums::{FeeType, InvoiceStatus, PaymentStatus};
use rago_core::models::{Fee, Invoice};
use rago_db::Db;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::scheduler::{charges_boundaries, load_charges};
use crate::JobError;

/// What a refresh did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Not a draft (or gone): nothing to do.
    NotDraft,
    /// Usage re-priced from events; `changed` when the totals moved.
    Repriced { changed: bool },
    /// Totals re-summed from the existing lines (see the module notes).
    Resummed { changed: bool },
}

/// Refresh one draft invoice. Returns the invoice as stored afterwards.
pub async fn refresh_draft_invoice(db: &Db, invoice_id: Uuid) -> Result<(Refresh, Option<Invoice>), JobError> {
    let Some(invoice) = db.find_invoice(invoice_id).await? else {
        return Ok((Refresh::NotDraft, None));
    };
    if invoice.status != InvoiceStatus::Draft {
        return Ok((Refresh::NotDraft, Some(invoice)));
    }
    let fees = db.fees_for_invoice(invoice.id).await?;
    let windows = usage_windows(&fees);
    let repriceable =
        invoice.progressive_billing_credit_amount_cents == 0 && invoice.coupons_amount_cents == 0 && windows.is_some();
    let Some(windows) = windows.filter(|_| repriceable) else {
        let before = invoice.total_amount_cents;
        let after = db.refresh_invoice_totals(invoice.id).await?;
        let changed = after.as_ref().is_some_and(|i| i.total_amount_cents != before);
        return Ok((Refresh::Resummed { changed }, after));
    };

    let currency = rago_core::money::Currency::new(invoice.currency.as_deref().unwrap_or("USD"));
    let mut new_usage: Vec<Fee> = Vec::new();
    for (subscription_id, (from, to, duration)) in &windows {
        let subscription = db.find_subscription(*subscription_id).await?;
        let plan = db.find_plan(subscription.plan_id).await?;
        let period = InvoicePeriod {
            fee_period: DayRange::new(from.date(), from.date()),
            fee_window: DayRange::new(from.date(), from.date()),
            charges_period: DayRange::new(from.date(), from.date() + chrono::Duration::days(duration.max(&1) - 1)),
            charges_window: DayRange::new(from.date(), to.date()),
        };
        let bounds = charges_boundaries(*from, *to, *duration);
        for loaded in load_charges(db, &subscription, &plan, *from, *to, *to).await? {
            if !loaded.charge.invoiceable {
                continue;
            }
            for fee in charge_lines(&loaded.input(), &subscription, &period, currency, Some(invoice.id))? {
                let mut props = fee.properties.as_object().cloned().unwrap_or_default();
                for (k, v) in bounds.as_object().into_iter().flatten() {
                    props.insert(k.clone(), v.clone());
                }
                new_usage.push(Fee {
                    properties: serde_json::Value::Object(props),
                    ..fee
                });
            }
        }
    }

    let removed: Vec<Uuid> = fees.iter().filter(|f| is_usage(f)).map(|f| f.id).collect();
    // The other lines are re-taxed with the new usage, from zero.
    let mut kept: Vec<Fee> = fees
        .into_iter()
        .filter(|f| !is_usage(f))
        .map(|f| Fee {
            taxes_amount_cents: 0,
            taxes_precise_amount_cents: Decimal::ZERO,
            taxes_rate: 0.0,
            applied_taxes: Vec::new(),
            ..f
        })
        .collect();
    let kept_len = kept.len();
    kept.extend(new_usage);
    let mut all = kept;

    let taxes = db.organization_taxes(invoice.organization_id).await?;
    let before = invoice.total_amount_cents;
    let mut refreshed = invoice.clone();
    assemble_invoice(&mut refreshed, &mut all, &taxes, &[]);
    // Prepaid credit drawn when the draft was made still covers it.
    refreshed.total_amount_cents = (refreshed.total_amount_cents - refreshed.prepaid_credit_amount_cents).max(0);
    refreshed.payment_status = if refreshed.total_amount_cents > 0 {
        PaymentStatus::Pending
    } else {
        PaymentStatus::Succeeded
    };

    let (others, usage) = all.split_at(kept_len);
    if !db.replace_draft_usage(&refreshed, &removed, usage, others).await? {
        return Ok((Refresh::NotDraft, db.find_invoice(invoice.id).await?));
    }
    let changed = refreshed.total_amount_cents != before;
    Ok((Refresh::Repriced { changed }, db.find_invoice(invoice.id).await?))
}

/// A usage line (a charge fee or its minimum true-up).
fn is_usage(f: &Fee) -> bool {
    matches!(f.fee_type, Some(FeeType::Charge))
}

/// The recorded usage window per subscription; `None` when a usage line has
/// none (it predates the recording) or the lines disagree.
fn usage_windows(fees: &[Fee]) -> Option<BTreeMap<Uuid, (NaiveDateTime, NaiveDateTime, i64)>> {
    let mut out: BTreeMap<Uuid, (NaiveDateTime, NaiveDateTime, i64)> = BTreeMap::new();
    let mut any = false;
    for f in fees.iter().filter(|f| is_usage(f)) {
        any = true;
        let sub = f.subscription_id?;
        let at = |k: &str| {
            f.properties
                .get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.naive_utc())
        };
        let window = (
            at("charges_from_datetime")?,
            at("charges_to_datetime")?,
            f.properties.get("charges_duration").and_then(|v| v.as_i64())?,
        );
        match out.get(&sub) {
            Some(existing) if *existing != window => return None,
            _ => {
                out.insert(sub, window);
            }
        }
    }
    any.then_some(out)
}

/// `Clock::RefreshDraftInvoicesJob`: refresh every draft; returns the drafts
/// whose totals changed (they announce `invoice.drafted` again).
pub async fn refresh_all_drafts(db: &Db) -> Result<Vec<Invoice>, JobError> {
    let mut changed = Vec::new();
    for id in db.draft_invoice_ids().await? {
        match refresh_draft_invoice(db, id).await? {
            (Refresh::Repriced { changed: true } | Refresh::Resummed { changed: true }, Some(invoice)) => {
                changed.push(invoice)
            }
            _ => {}
        }
    }
    Ok(changed)
}

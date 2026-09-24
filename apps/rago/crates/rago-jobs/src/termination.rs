//! Final billing of a subscription terminated mid-period
//! (`Subscriptions::TerminateService` → `BillSubscriptionJob` with
//! `invoicing_reason: subscription_terminating` →
//! `CreditNotes::CreateFromTermination`).
//!
//! * **Pay in arrears** — the fee for the days from the period start (or the
//!   subscription start / trial end) through the termination day, prorated at
//!   `plan_amount / period_days`, plus the usage up to the termination.
//! * **Pay in advance** — the period's fee was collected upfront: the final
//!   invoice bills the usage up to the termination only, and the unused days
//!   of the upfront fee are credited back on a credit note against the invoice
//!   that billed it (taxes and coupon share included). A period whose upfront
//!   fee was never billed is billed for the days lived instead.
//!
//! A termination on a rollover day after midnight first bills the period that
//! closed that day (guarded, so a billing run that already did is not
//! repeated). Every step is idempotent: invoices behind the per-period re-bill
//! guard, the credit note behind the fee's credited amount.

use chrono::{NaiveDateTime, Utc};
use rago_billing::dates::due_for_billing;
use rago_billing::proration::{termination_credit, SubscriptionSpan};
use rago_core::enums::InvoiceStatus;
use rago_core::models::{CreditNote, CreditNoteItem, Fee, Invoice, Plan, Subscription};
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::HttpTransport;
use rust_decimal::prelude::ToPrimitive;
use serde_json::json;
use uuid::Uuid;

use crate::scheduler::{
    bill_and_collect, bill_closing_charges_and_collect, bill_terminating_and_collect, span_of,
    BillOutcome,
};
use crate::webhook_emit::{record_credit_note_generated, record_webhook};
use crate::JobError;

/// Lago `CreditNote.reason` `order_cancellation`.
const REASON_ORDER_CANCELLATION: i32 = 3;

use rago_db::credit_note_credits::{CREDIT_NOTE_STATUS_DRAFT, CREDIT_NOTE_STATUS_FINALIZED};

/// The status a termination credit note takes from the invoice that billed
/// the upfront fee: finalized on a finalized invoice, draft on a draft one
/// (finalized with it), none otherwise.
pub(crate) fn termination_credit_note_status(invoice_status: InvoiceStatus) -> Option<i32> {
    match invoice_status {
        InvoiceStatus::Finalized => Some(CREDIT_NOTE_STATUS_FINALIZED),
        InvoiceStatus::Draft => Some(CREDIT_NOTE_STATUS_DRAFT),
        _ => None,
    }
}

/// What the final billing of a mid-period termination produced.
#[derive(Debug, Default)]
pub struct TerminationBilling {
    /// The final invoice (`None` when a pay-in-advance subscription had no
    /// usage to bill).
    pub invoice: Option<BillOutcome>,
    /// The credit note for the unused days of the upfront fee.
    pub credit_note_id: Option<Uuid>,
}

/// Bill `subscription` terminated at `end`, which is not a billing boundary
/// (00:00 of a rollover day is billed as a whole closing period instead).
pub async fn bill_mid_period_termination<T: HttpTransport>(
    db: &Db,
    transport: &T,
    subscription: &Subscription,
    plan: &Plan,
    end: NaiveDateTime,
    charge: bool,
) -> Result<TerminationBilling, JobError> {
    let end_date = end.date();
    let span = span_of(subscription, plan, end_date);
    if span.started_on.is_some_and(|s| s > end_date) {
        return Ok(TerminationBilling::default());
    }

    // The period that closed on the termination day (the regular run skips a
    // pay-in-advance subscription leaving that day).
    if due_for_billing(span.interval, span.anniversary, span.subscription_at, end_date)
        && span.started_on.is_some_and(|s| s < end_date)
    {
        if plan.pay_in_advance {
            bill_closing_charges_and_collect(db, transport, subscription, plan, end_date, charge).await?;
        } else {
            bill_and_collect(
                db,
                transport,
                subscription,
                plan,
                span.interval,
                span.anniversary,
                end_date,
                charge,
            )
            .await?;
        }
    }

    let period = span.period_of(end_date);
    let upfront = if plan.pay_in_advance {
        db.subscription_fee_for_period(
            subscription.id,
            period.from.and_hms_opt(0, 0, 0).unwrap_or_default(),
            period.to.and_hms_opt(23, 59, 59).unwrap_or_default(),
        )
        .await?
    } else {
        None
    };
    // Credit first, then bill (Lago's TerminateService order): the final
    // invoice then consumes the credit-note balance when it is issued.
    let credit_note_id = match &upfront {
        Some((fee, fee_invoice)) => {
            credit_unused_advance_days(db, &span, plan, end, fee, fee_invoice).await?
        }
        None => None,
    };
    let invoice = bill_terminating_and_collect(
        db,
        transport,
        subscription,
        plan,
        end,
        upfront.is_some(),
        charge,
    )
    .await?;
    Ok(TerminationBilling { invoice, credit_note_id })
}

/// `CreditNotes::CreateFromTermination`: credit the days of the upfront fee
/// after the termination day.
///
/// * finalized upfront invoice — a finalized credit note, announced now and
///   spendable on the customer's next invoices;
/// * draft upfront invoice (its grace period is still open) — a *draft*
///   credit note on the draft. It is finalized, numbered and announced
///   together with the invoice, and only then becomes spendable, so the
///   termination credit is never lost to the grace period;
/// * any other status (voided, ...) — nothing to credit.
async fn credit_unused_advance_days(
    db: &Db,
    span: &SubscriptionSpan,
    plan: &Plan,
    end: NaiveDateTime,
    fee: &Fee,
    invoice: &Invoice,
) -> Result<Option<Uuid>, JobError> {
    let note_status = match termination_credit_note_status(invoice.status) {
        Some(status) => status,
        None => {
            if fee.amount_cents > 0 && span.unused_advance_days(end.date()) > 0 {
                tracing::warn!(
                    invoice_id = %invoice.id,
                    status = invoice.status.as_str(),
                    "upfront fee invoice is neither draft nor finalized; no termination credit note issued"
                );
            }
            return Ok(None);
        }
    };
    let period = span.period_of(end.date());
    let plan_amount_cents = fee
        .amount_details
        .get("plan_amount_cents")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| plan.amount_cents.unwrap_or(0));
    let already = db.credited_amount_on_fee(fee.id).await?;
    let Some(credit) = termination_credit(
        plan_amount_cents,
        period.days(),
        span.unused_advance_days(end.date()),
        fee.amount_cents,
        already,
        fee.precise_coupons_amount_cents,
        fee.taxes_rate,
    ) else {
        return Ok(None);
    };

    let now = Utc::now();
    let currency = fee.amount_currency.clone();
    let id = Uuid::new_v4();
    let mut note = CreditNote {
        id,
        organization_id: invoice.organization_id,
        customer_id: invoice.customer_id.unwrap_or_default(),
        invoice_id: invoice.id,
        sequential_id: 0,
        number: String::new(),
        issuing_date: end.date(),
        reason: REASON_ORDER_CANCELLATION,
        credit_status: Some(0),
        refund_status: None,
        status: note_status,
        credit_amount_cents: credit.total_amount_cents,
        credit_amount_currency: currency.clone(),
        balance_amount_cents: credit.total_amount_cents,
        refund_amount_cents: 0,
        total_amount_cents: credit.total_amount_cents,
        total_amount_currency: currency.clone(),
        taxes_amount_cents: credit.taxes_amount_cents,
        taxes_rate: fee.taxes_rate,
        coupons_adjustment_amount_cents: rago_core::money::round_to_integer(credit.coupons_adjustment_cents)
            .to_i64()
            .unwrap_or(0),
        precise_coupons_adjustment_amount_cents: credit.coupons_adjustment_cents,
        precise_taxes_amount_cents: credit.precise_taxes_amount_cents,
        created_at: now,
        updated_at: now,
        voided_at: None,
        items: vec![CreditNoteItem {
            id: Uuid::new_v4(),
            organization_id: invoice.organization_id,
            credit_note_id: Some(id),
            fee_id: fee.id,
            amount_cents: credit.item_amount_cents,
            precise_amount_cents: credit.precise_item_amount_cents,
            amount_currency: currency.clone(),
        }],
        applied_taxes: Vec::new(),
    };
    if !db.insert_fee_credit_note(&mut note, fee.id, already).await? {
        // Credited meanwhile by a concurrent run; the next run recomputes.
        return Ok(None);
    }
    if note_status == CREDIT_NOTE_STATUS_DRAFT {
        // Announced when the invoice is finalized (see `crate::finalize`).
        tracing::info!(
            credit_note_id = %note.id,
            invoice_id = %invoice.id,
            credit_amount_cents = note.credit_amount_cents,
            "termination credit note issued as a draft on a draft invoice"
        );
        return Ok(Some(note.id));
    }
    let object = json!({
        "lago_id": note.id,
        "number": note.number,
        "lago_invoice_id": note.invoice_id,
        "reason": "order_cancellation",
        "credit_amount_cents": note.credit_amount_cents,
        "taxes_amount_cents": note.taxes_amount_cents,
        "total_amount_cents": note.total_amount_cents,
        "currency": currency,
    });
    record_webhook(db, note.organization_id, WebhookKind::CreditNoteCreated, object.clone()).await?;
    record_credit_note_generated(db, note.organization_id, note.id, object).await?;
    Ok(Some(note.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_credit_follows_the_upfront_invoice_status() {
        assert_eq!(
            termination_credit_note_status(InvoiceStatus::Finalized),
            Some(CREDIT_NOTE_STATUS_FINALIZED)
        );
        assert_eq!(
            termination_credit_note_status(InvoiceStatus::Draft),
            Some(CREDIT_NOTE_STATUS_DRAFT)
        );
        for status in [
            InvoiceStatus::Voided,
            InvoiceStatus::Generating,
            InvoiceStatus::Failed,
            InvoiceStatus::Deleted,
        ] {
            assert_eq!(termination_credit_note_status(status), None);
        }
    }
}

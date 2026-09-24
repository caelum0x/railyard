//! Partial-period billing: which days of a subscription period one invoice
//! bills, for every interval and billing time.
//!
//! Port of the proration inputs of `Subscriptions::DatesService` +
//! `Fees::SubscriptionService` for the three partial-period cases:
//!
//! * **first period** — a subscription starting mid-period (calendar billing)
//!   is billed the days from its start to the period end: at the next boundary
//!   in arrears, on the start day in advance (`invoicing_reason:
//!   subscription_starting`);
//! * **trial** — the fee is owed from the trial end only;
//! * **termination** — a subscription ending mid-period (`ending_at` not on a
//!   boundary) is billed, in arrears, the fee and the usage from the period
//!   start through the termination day; in advance, the usage through the
//!   termination day only (its fee was collected upfront) and the unused days
//!   of that fee are credited back (`CreditNotes::CreateFromTermination`).
//!
//! A day price is always `plan_amount / full period length in days`, so a
//! prorated fee is `billed_days * plan_amount / period_days` whatever the
//! interval (weekly .. yearly).

use chrono::{Duration, NaiveDate};
use rago_core::money::round_to_integer;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::dates::{billing_boundaries, period_containing, Interval, MonthlyDates, PeriodDates};

/// The period dates the fee builders need: the day price of the
/// subscription-fee period and the usage-charges window (for the prorated
/// `min_amount_cents` top-up). Implemented by the ported `MonthlyDates` /
/// `PeriodDates` (whole periods) and by [`InvoicePeriod`] (any interval,
/// partial periods).
pub trait FeeDates {
    /// `DatesService#single_day_price` — plan amount over the length of the
    /// subscription-fee period.
    fn single_day_price(&self, plan_amount_cents: i64) -> Decimal;
    /// First billable day of the usage-charges window.
    fn charges_from_date(&self) -> NaiveDate;
    /// Last billable day of the usage-charges window.
    fn charges_to_date(&self) -> NaiveDate;
    /// Length in days of the whole usage-charges period (the proration base of
    /// a charge minimum).
    fn charges_duration_in_days(&self) -> i64;
}

impl FeeDates for MonthlyDates {
    fn single_day_price(&self, plan_amount_cents: i64) -> Decimal {
        MonthlyDates::single_day_price(self, plan_amount_cents)
    }
    fn charges_from_date(&self) -> NaiveDate {
        self.compute_charges_from_date()
    }
    fn charges_to_date(&self) -> NaiveDate {
        self.compute_charges_to_date()
    }
    fn charges_duration_in_days(&self) -> i64 {
        MonthlyDates::charges_duration_in_days(self)
    }
}

impl FeeDates for PeriodDates {
    fn single_day_price(&self, plan_amount_cents: i64) -> Decimal {
        PeriodDates::single_day_price(self, plan_amount_cents)
    }
    fn charges_from_date(&self) -> NaiveDate {
        self.boundaries().charges_from_date
    }
    fn charges_to_date(&self) -> NaiveDate {
        self.boundaries().charges_to_date
    }
    fn charges_duration_in_days(&self) -> i64 {
        self.boundaries().charges_duration_days
    }
}

/// An inclusive day range; empty when `from > to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

impl DayRange {
    pub fn new(from: NaiveDate, to: NaiveDate) -> Self {
        Self { from, to }
    }

    /// Number of days in the range (0 when empty).
    pub fn days(&self) -> i64 {
        ((self.to - self.from).num_days() + 1).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.days() == 0
    }

    /// The range starting no earlier than `day`.
    fn starting_no_earlier_than(self, day: Option<NaiveDate>) -> Self {
        match day {
            Some(d) if d > self.from => Self { from: d, to: self.to },
            _ => self,
        }
    }

    /// The range ending no later than `day`.
    fn ending_no_later_than(self, day: NaiveDate) -> Self {
        Self { from: self.from, to: self.to.min(day) }
    }
}

/// The dates of one subscription invoice: the full subscription-fee period
/// (the re-bill guard key) and the days of it billed, the full usage period
/// and the days of usage billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvoicePeriod {
    /// The whole period the subscription fee belongs to.
    pub fee_period: DayRange,
    /// The days of `fee_period` billed (empty: no fee on this invoice).
    pub fee_window: DayRange,
    /// The whole usage-charges period.
    pub charges_period: DayRange,
    /// The days of usage billed (empty: no usage on this invoice).
    pub charges_window: DayRange,
}

impl InvoicePeriod {
    /// How to bill the subscription fee: in full, prorated to the billed days,
    /// or not at all.
    pub fn fee_proration(&self) -> crate::subscription_fee::Proration {
        use crate::subscription_fee::Proration;
        let billed = self.fee_window.days();
        if billed == 0 {
            Proration::Zero
        } else if billed >= self.fee_period.days() {
            Proration::Full
        } else {
            Proration::Prorated { days_to_bill: billed }
        }
    }
}

impl FeeDates for InvoicePeriod {
    fn single_day_price(&self, plan_amount_cents: i64) -> Decimal {
        let days = self.fee_period.days();
        if days == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(plan_amount_cents) / Decimal::from(days)
    }
    fn charges_from_date(&self) -> NaiveDate {
        self.charges_window.from
    }
    fn charges_to_date(&self) -> NaiveDate {
        self.charges_window.to
    }
    fn charges_duration_in_days(&self) -> i64 {
        self.charges_period.days()
    }
}

/// A subscription's billing shape and lifetime, enough to date any of its
/// invoices.
#[derive(Debug, Clone, Copy)]
pub struct SubscriptionSpan {
    pub interval: Interval,
    /// `true` for anniversary billing, `false` for calendar.
    pub anniversary: bool,
    pub pay_in_advance: bool,
    /// The anniversary anchor (`subscription_at`).
    pub subscription_at: NaiveDate,
    /// First day the subscription was live (`started_at`); usage and fee
    /// before it are never billed.
    pub started_on: Option<NaiveDate>,
    /// First day the subscription fee is owed when later than the start (the
    /// free-trial end day).
    pub fee_starts_on: Option<NaiveDate>,
}

impl SubscriptionSpan {
    fn period_containing(&self, date: NaiveDate) -> DayRange {
        let (from, to) = period_containing(self.interval, self.anniversary, self.subscription_at, date);
        DayRange::new(from, to)
    }

    fn first_fee_day(&self) -> Option<NaiveDate> {
        match (self.started_on, self.fee_starts_on) {
            (Some(s), Some(f)) => Some(s.max(f)),
            (s, f) => s.or(f),
        }
    }

    /// The invoice of a run on `billing_at`: in arrears the fee and usage of
    /// the period that closed; in advance the fee of the period containing
    /// `billing_at` and the usage of the period before it. Each window starts
    /// no earlier than the subscription (fee: its trial end), so a first
    /// period started mid-way is prorated and a mid-period run of a
    /// pay-in-advance subscription on its start day bills the rest of that
    /// period.
    pub fn periodic(&self, billing_at: NaiveDate) -> InvoicePeriod {
        let b = billing_boundaries(
            self.interval,
            self.anniversary,
            self.subscription_at,
            billing_at,
            self.pay_in_advance,
        );
        let fee_period = DayRange::new(b.from_date, b.to_date);
        let charges_period = DayRange::new(b.charges_from_date, b.charges_to_date);
        InvoicePeriod {
            fee_period,
            fee_window: fee_period.starting_no_earlier_than(self.first_fee_day()),
            charges_period,
            charges_window: charges_period.starting_no_earlier_than(self.started_on),
        }
    }

    /// The in-progress period containing `date` (invoice previews and
    /// operator-triggered current-period invoices): fee and usage of that
    /// period, each starting no earlier than the subscription (fee: its trial
    /// end).
    pub fn current(&self, date: NaiveDate) -> InvoicePeriod {
        let period = self.period_containing(date);
        InvoicePeriod {
            fee_period: period,
            fee_window: period.starting_no_earlier_than(self.first_fee_day()),
            charges_period: period,
            charges_window: period.starting_no_earlier_than(self.started_on),
        }
    }

    /// The final invoice of a subscription terminated on `end` (the
    /// termination day is billed, `Subscriptions::DatesService` with
    /// `terminated_at`): usage from the period start through `end`; in arrears
    /// also the fee for those days, in advance no fee (it was collected at the
    /// start of the period; see [`Self::unused_advance_days`]).
    pub fn terminating(&self, end: NaiveDate) -> InvoicePeriod {
        let period = self.period_containing(end);
        let lived = period.starting_no_earlier_than(self.started_on).ending_no_later_than(end);
        let fee_window = if self.pay_in_advance {
            DayRange::new(period.from, period.from - Duration::days(1))
        } else {
            period
                .starting_no_earlier_than(self.first_fee_day())
                .ending_no_later_than(end)
        };
        InvoicePeriod {
            fee_period: period,
            fee_window,
            charges_period: period,
            charges_window: lived,
        }
    }

    /// Days of the pay-in-advance period containing `end` that follow the
    /// termination day — the part of the upfront fee to credit back
    /// (`CreditNotes::CreateFromTermination#compute_amount`).
    pub fn unused_advance_days(&self, end: NaiveDate) -> i64 {
        let period = self.period_containing(end);
        (period.to - end).num_days().max(0)
    }

    /// Whether `date` still falls in the subscription's first period (the one
    /// containing its start day).
    pub fn in_first_period(&self, date: NaiveDate) -> bool {
        match self.started_on {
            Some(start) => date >= start && self.period_containing(date) == self.period_containing(start),
            None => false,
        }
    }

    /// The whole period containing `date`.
    pub fn period_of(&self, date: NaiveDate) -> DayRange {
        self.period_containing(date)
    }
}

/// The credit note owed for the unused days of a pay-in-advance subscription
/// fee when the subscription terminates mid-period
/// (`CreditNotes::CreateFromTermination` + `CreditNotes::ApplyTaxesService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationCredit {
    /// The credited fee amount (credit-note item), excluding taxes.
    pub item_amount_cents: i64,
    pub precise_item_amount_cents: Decimal,
    /// Coupon share of the item (the fee's coupon discount, pro rata).
    pub coupons_adjustment_cents: Decimal,
    pub taxes_amount_cents: i64,
    pub precise_taxes_amount_cents: Decimal,
    /// `item - coupons + taxes`, rounded — the credit note total.
    pub total_amount_cents: i64,
}

/// The unused-days credit of a pay-in-advance fee.
///
/// * `plan_amount_cents` / `period_days` — the day price of the fee's period;
/// * `unused_days` — days of that period after the termination day;
/// * `fee_amount_cents` — what the fee actually billed (a first period may
///   have been prorated: the credit never exceeds it);
/// * `already_credited_cents` — credit-note items already issued on the fee
///   (deducted, so a re-run or an earlier manual credit is not repeated);
/// * `fee_coupons_precise_cents` / `fee_taxes_rate` — the fee's coupon
///   discount and tax rate (percent), applied pro rata to the credited part.
///
/// `None` when nothing is owed.
pub fn termination_credit(
    plan_amount_cents: i64,
    period_days: i64,
    unused_days: i64,
    fee_amount_cents: i64,
    already_credited_cents: i64,
    fee_coupons_precise_cents: Decimal,
    fee_taxes_rate: f64,
) -> Option<TerminationCredit> {
    if period_days <= 0 || unused_days <= 0 || fee_amount_cents <= 0 {
        return None;
    }
    let day_price = Decimal::from(plan_amount_cents) / Decimal::from(period_days);
    let mut precise = day_price * Decimal::from(unused_days) - Decimal::from(already_credited_cents);
    let remaining_on_fee = Decimal::from(fee_amount_cents - already_credited_cents);
    if precise > remaining_on_fee {
        precise = remaining_on_fee;
    }
    let item_amount_cents = round_to_integer(precise).to_i64().unwrap_or(0);
    if item_amount_cents <= 0 {
        return None;
    }
    let precise_item = Decimal::from(item_amount_cents);
    let coupons = fee_coupons_precise_cents.max(Decimal::ZERO) * precise_item / Decimal::from(fee_amount_cents);
    let rate = Decimal::from_f64_retain(fee_taxes_rate).unwrap_or(Decimal::ZERO);
    let precise_taxes = (precise_item - coupons) * rate / Decimal::from(100);
    let taxes_amount_cents = round_to_integer(precise_taxes).to_i64().unwrap_or(0);
    let total_amount_cents = round_to_integer(precise_item - coupons + precise_taxes)
        .to_i64()
        .unwrap_or(0);
    Some(TerminationCredit {
        item_amount_cents,
        precise_item_amount_cents: precise_item,
        coupons_adjustment_cents: coupons,
        taxes_amount_cents,
        precise_taxes_amount_cents: precise_taxes,
        total_amount_cents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription_fee::Proration;
    use rust_decimal_macros::dec;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn span(interval: Interval, anniversary: bool, advance: bool, sub: NaiveDate) -> SubscriptionSpan {
        SubscriptionSpan {
            interval,
            anniversary,
            pay_in_advance: advance,
            subscription_at: sub,
            started_on: Some(sub),
            fee_starts_on: None,
        }
    }

    fn fee(plan: i64, p: &InvoicePeriod) -> Decimal {
        crate::subscription_fee::compute_precise_amount_cents(plan, p, p.fee_proration())
    }

    #[test]
    fn arrears_termination_bills_fee_and_usage_through_the_termination_day() {
        // Monthly calendar, $31.00, terminated Mar 10: 10 of 31 days.
        let s = span(Interval::Monthly, false, false, d(2026, 1, 1));
        let p = s.terminating(d(2026, 3, 10));
        assert_eq!(p.fee_period, DayRange::new(d(2026, 3, 1), d(2026, 3, 31)));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 1), d(2026, 3, 10)));
        assert_eq!(p.charges_window, p.fee_window);
        assert!(matches!(p.fee_proration(), Proration::Prorated { days_to_bill: 10 }));
        assert_eq!(fee(3_100, &p), dec!(1000));
    }

    #[test]
    fn arrears_termination_on_quarterly_and_yearly_plans_uses_the_whole_period_day_price() {
        // Quarterly calendar Q1 2026 = 90 days; terminated Feb 14 = 45 days.
        let q = span(Interval::Quarterly, false, false, d(2025, 11, 3)).terminating(d(2026, 2, 14));
        assert_eq!(q.fee_period.days(), 90);
        assert_eq!(q.fee_window.days(), 45);
        assert_eq!(fee(9_000, &q), dec!(4500));
        assert_eq!(q.charges_duration_in_days(), 90);
        // Yearly anniversary from 2025-06-10: period 2026-06-10..2027-06-09
        // (365 days); terminated 2026-07-09 = 30 days.
        let y = span(Interval::Yearly, true, false, d(2025, 6, 10)).terminating(d(2026, 7, 9));
        assert_eq!(y.fee_period, DayRange::new(d(2026, 6, 10), d(2027, 6, 9)));
        assert_eq!(y.fee_window.days(), 30);
        assert_eq!(fee(36_500, &y), dec!(3000));
        // Semiannual calendar H2: Jul 1 .. Dec 31 = 184 days.
        let h = span(Interval::Semiannual, false, false, d(2026, 1, 1)).terminating(d(2026, 7, 1));
        assert_eq!((h.fee_period.days(), h.fee_window.days()), (184, 1));
    }

    #[test]
    fn termination_in_the_first_period_bills_only_the_lived_days() {
        let mut s = span(Interval::Monthly, false, false, d(2026, 3, 11));
        s.started_on = Some(d(2026, 3, 11));
        let p = s.terminating(d(2026, 3, 20));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 11), d(2026, 3, 20)));
        assert_eq!(p.charges_window, p.fee_window);
        assert_eq!(fee(3_100, &p), dec!(1000));
    }

    #[test]
    fn advance_termination_bills_usage_only_and_credits_unused_days() {
        let s = span(Interval::Monthly, true, true, d(2026, 1, 15));
        // Period Mar 15 .. Apr 14 (31 days); terminated Mar 24.
        let p = s.terminating(d(2026, 3, 24));
        assert_eq!(p.fee_period, DayRange::new(d(2026, 3, 15), d(2026, 4, 14)));
        assert!(p.fee_window.is_empty());
        assert!(matches!(p.fee_proration(), Proration::Zero));
        assert_eq!(p.charges_window, DayRange::new(d(2026, 3, 15), d(2026, 3, 24)));
        assert_eq!(s.unused_advance_days(d(2026, 3, 24)), 21);
        // $31.00 plan: 21 unused days = $21.00, 20% tax on top.
        let c = termination_credit(3_100, 31, 21, 3_100, 0, Decimal::ZERO, 20.0).unwrap();
        assert_eq!(c.item_amount_cents, 2_100);
        assert_eq!(c.taxes_amount_cents, 420);
        assert_eq!(c.total_amount_cents, 2_520);
        // Terminated on the last day: nothing unused.
        assert_eq!(s.unused_advance_days(d(2026, 4, 14)), 0);
    }

    #[test]
    fn advance_yearly_termination_credit() {
        // Yearly calendar 2026 (365 days), $365.00, terminated Mar 31 (day 90):
        // 275 unused days.
        let s = span(Interval::Yearly, false, true, d(2025, 5, 5));
        assert_eq!(s.unused_advance_days(d(2026, 3, 31)), 275);
        let c = termination_credit(36_500, 365, 275, 36_500, 0, Decimal::ZERO, 0.0).unwrap();
        assert_eq!((c.item_amount_cents, c.total_amount_cents), (27_500, 27_500));
    }

    #[test]
    fn termination_credit_is_capped_and_deducts_earlier_credits() {
        // First period was prorated to $10.00; the unused part cannot exceed it.
        let c = termination_credit(3_100, 31, 20, 1_000, 0, Decimal::ZERO, 0.0).unwrap();
        assert_eq!(c.item_amount_cents, 1_000);
        // 21 unused days = 2100, 600 already credited → 1500.
        let c = termination_credit(3_100, 31, 21, 3_100, 600, Decimal::ZERO, 0.0).unwrap();
        assert_eq!(c.item_amount_cents, 1_500);
        // A re-run after the credit was issued owes nothing.
        assert!(termination_credit(3_100, 31, 21, 3_100, 2_100, Decimal::ZERO, 0.0).is_none());
        assert!(termination_credit(3_100, 31, 0, 3_100, 0, Decimal::ZERO, 0.0).is_none());
        // A fee discounted by a $3.10 coupon: the credit carries 2100/3100 of it.
        let c = termination_credit(3_100, 31, 21, 3_100, 0, dec!(310), 10.0).unwrap();
        assert_eq!(c.coupons_adjustment_cents, dec!(210));
        assert_eq!(c.taxes_amount_cents, 189);
        assert_eq!(c.total_amount_cents, 2_079);
    }

    #[test]
    fn calendar_advance_start_mid_period_bills_the_rest_of_the_period() {
        // Monthly calendar in advance started Mar 11: Mar 11..31 = 21/31 days.
        let s = span(Interval::Monthly, false, true, d(2026, 3, 11));
        assert!(s.in_first_period(d(2026, 3, 11)));
        let p = s.periodic(d(2026, 3, 11));
        assert_eq!(p.fee_period, DayRange::new(d(2026, 3, 1), d(2026, 3, 31)));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 11), d(2026, 3, 31)));
        assert!(p.charges_window.is_empty(), "no usage before the start");
        assert_eq!(fee(3_100, &p), dec!(2100));
        // Quarterly calendar started Feb 15: Feb 15 .. Mar 31 = 45 of 90 days.
        let q = span(Interval::Quarterly, false, true, d(2026, 2, 15)).periodic(d(2026, 2, 15));
        assert_eq!(q.fee_window, DayRange::new(d(2026, 2, 15), d(2026, 3, 31)));
        assert_eq!(fee(9_000, &q), dec!(4500));
        // Yearly calendar started Jul 2 2026: 183 of 365 days.
        let y = span(Interval::Yearly, false, true, d(2026, 7, 2)).periodic(d(2026, 7, 2));
        assert_eq!(y.fee_window.days(), 183);
        // The next boundary bills the following period in full.
        let next = s.periodic(d(2026, 4, 1));
        assert!(matches!(next.fee_proration(), Proration::Full));
        assert_eq!(next.charges_window, DayRange::new(d(2026, 3, 11), d(2026, 3, 31)));
        assert!(!s.in_first_period(d(2026, 4, 1)));
    }

    #[test]
    fn anniversary_advance_start_bills_a_full_period() {
        let s = span(Interval::Quarterly, true, true, d(2026, 3, 11));
        let p = s.periodic(d(2026, 3, 11));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 11), d(2026, 6, 10)));
        assert!(matches!(p.fee_proration(), Proration::Full));
    }

    #[test]
    fn arrears_first_period_is_prorated_from_the_start() {
        let s = span(Interval::Monthly, false, false, d(2026, 3, 11));
        let p = s.periodic(d(2026, 4, 1));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 11), d(2026, 3, 31)));
        assert_eq!(fee(3_100, &p), dec!(2100));
        assert_eq!(p.charges_window, p.fee_window);
        assert_eq!(p.charges_period.days(), 31);
    }

    #[test]
    fn trial_end_defers_the_fee_not_the_usage() {
        let mut s = span(Interval::Monthly, false, false, d(2026, 3, 1));
        s.fee_starts_on = Some(d(2026, 3, 22));
        let p = s.periodic(d(2026, 4, 1));
        assert_eq!(p.fee_window, DayRange::new(d(2026, 3, 22), d(2026, 3, 31)));
        assert_eq!(p.charges_window, DayRange::new(d(2026, 3, 1), d(2026, 3, 31)));
        // A trial covering the whole period bills no fee.
        s.fee_starts_on = Some(d(2026, 4, 5));
        assert!(matches!(s.periodic(d(2026, 4, 1)).fee_proration(), Proration::Zero));
    }

    /// For every interval and billing time, the days billed by a mid-period
    /// termination plus the days credited back (advance) or never billed
    /// (arrears) always add up to the whole period.
    #[test]
    fn termination_partitions_every_period() {
        for interval in [Interval::Weekly, Interval::Monthly, Interval::Quarterly, Interval::Semiannual, Interval::Yearly] {
            for anniversary in [false, true] {
                let sub = d(2024, 1, 31);
                let s = span(interval, anniversary, true, sub);
                let mut end = d(2025, 3, 1);
                while end < d(2027, 3, 1) {
                    let p = s.terminating(end);
                    assert!(p.fee_period.from <= end && end <= p.fee_period.to);
                    assert_eq!(
                        p.charges_window.days() + s.unused_advance_days(end),
                        p.fee_period.days(),
                        "{interval:?} anniversary={anniversary} end={end}"
                    );
                    end += Duration::days(5);
                }
            }
        }
    }
}

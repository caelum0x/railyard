//! Subscription billing-period boundaries.
//!
//! Faithful port of `Subscriptions::DatesService` + `Dates::MonthlyService`
//! for the monthly interval (calendar and anniversary billing), covering the
//! standard non-terminated period plus proration inputs (`single_day_price`,
//! `charges_duration`). Dates are computed at day granularity; the Ruby
//! customer-timezone shift is applied by callers when converting a date to a
//! UTC datetime (noted as a simplification — the day-level period math matches).
//!
//! Every interval (weekly, monthly, quarterly, semiannual, yearly) and both
//! billing times are covered by [`period_containing`] / [`billing_boundaries`]
//! (see the interval section below); the generic walk is proven identical to
//! `MonthlyDates` on every monthly rollover.

use chrono::{Datelike, Duration, NaiveDate};

/// The six period boundaries + the charges duration in days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boundaries {
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub charges_from_date: NaiveDate,
    pub charges_to_date: NaiveDate,
    pub charges_duration_days: i64,
}

/// `Subscriptions::Dates::MonthlyService` inputs.
#[derive(Debug, Clone, Copy)]
pub struct MonthlyDates {
    pub subscription_at: NaiveDate,
    pub billing_at: NaiveDate,
    /// `true` for anniversary billing, `false` for calendar.
    pub anniversary: bool,
    pub pay_in_advance: bool,
    pub current_usage: bool,
}

impl MonthlyDates {
    fn calendar(&self) -> bool {
        !self.anniversary
    }

    fn subscription_day(&self) -> u32 {
        self.subscription_at.day()
    }

    /// `#base_date`.
    fn base_date(&self) -> NaiveDate {
        if self.current_usage {
            self.billing_at
        } else {
            self.compute_base_date()
        }
    }

    /// `MonthlyService#compute_base_date`.
    fn compute_base_date(&self) -> NaiveDate {
        let billing_date = self.billing_at;
        if self.anniversary
            && last_day_of_month(billing_date)
            && billing_date.day() < self.subscription_day()
        {
            let prev = add_months(billing_date, -1);
            if end_of_month(prev).day() >= self.subscription_day() {
                return build_date(
                    prev.year(),
                    prev.month() as i32,
                    self.subscription_day() as i32,
                );
            }
            return end_of_month(prev);
        }
        add_months(billing_date, -1)
    }

    /// `MonthlyService#compute_from_date`.
    pub fn compute_from_date(&self) -> NaiveDate {
        let date = self.base_date();
        if self.pay_in_advance {
            return if self.anniversary {
                self.previous_anniversary_day(self.billing_at)
            } else {
                beginning_of_month(self.billing_at)
            };
        }
        if self.anniversary {
            self.previous_anniversary_day(date)
        } else {
            beginning_of_month(date)
        }
    }

    /// `MonthlyService#compute_to_date`.
    pub fn compute_to_date(&self, from_date: NaiveDate) -> NaiveDate {
        if self.calendar() || self.subscription_day() == 1 {
            return end_of_month(from_date);
        }
        let mut year = from_date.year();
        let mut month = from_date.month() as i32 + 1;
        let day = self.subscription_day() as i32 - 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
        let date = build_date(year, month, day);
        if last_day_of_month(date) && self.subscription_day() as i32 > date.day() as i32 {
            return date - Duration::days(1);
        }
        date
    }

    /// `MonthlyService#compute_charges_from_date` (non-terminated).
    pub fn compute_charges_from_date(&self) -> NaiveDate {
        if !self.pay_in_advance {
            return self.compute_from_date();
        }
        if self.calendar() {
            return beginning_of_month(self.base_date());
        }
        self.previous_anniversary_day(self.base_date())
    }

    /// `MonthlyService#compute_charges_to_date`.
    pub fn compute_charges_to_date(&self) -> NaiveDate {
        if self.calendar() {
            return end_of_month(self.compute_charges_from_date());
        }
        self.compute_to_date(self.compute_charges_from_date())
    }

    /// `MonthlyService#compute_duration` — the whole period length in days.
    pub fn compute_duration(&self, from_date: NaiveDate) -> i64 {
        let period_start = self.compute_previous_beginning_of_period(from_date);
        let period_end = self.compute_to_date(period_start);
        (period_end + Duration::days(1) - period_start).num_days()
    }

    /// `#charges_duration_in_days`.
    pub fn charges_duration_in_days(&self) -> i64 {
        self.compute_duration(self.compute_charges_from_date())
    }

    /// `MonthlyService#compute_previous_beginning_of_period`.
    fn compute_previous_beginning_of_period(&self, date: NaiveDate) -> NaiveDate {
        if self.calendar() {
            beginning_of_month(date)
        } else {
            self.previous_anniversary_day(date)
        }
    }

    /// `MonthlyService#previous_anniversary_day`.
    fn previous_anniversary_day(&self, date: NaiveDate) -> NaiveDate {
        let day = if self.anniversary
            && last_day_of_month(date)
            && date.day() < self.subscription_day()
        {
            date.day()
        } else {
            self.subscription_day()
        };

        let (year, month) = if date.day() < day {
            if date.month() == 1 {
                (date.year() - 1, 12)
            } else {
                (date.year(), date.month() as i32 - 1)
            }
        } else {
            (date.year(), date.month() as i32)
        };

        build_date(year, month, day as i32)
    }

    /// Compute all boundaries for the current period.
    pub fn boundaries(&self) -> Boundaries {
        let from_date = self.compute_from_date();
        let to_date = self.compute_to_date(from_date);
        let charges_from_date = self.compute_charges_from_date();
        let charges_to_date = self.compute_charges_to_date();
        Boundaries {
            from_date,
            to_date,
            charges_from_date,
            charges_to_date,
            charges_duration_days: self.charges_duration_in_days(),
        }
    }

    /// `DatesService#single_day_price` = `plan_amount_cents / duration`.
    pub fn single_day_price(&self, plan_amount_cents: i64) -> rust_decimal::Decimal {
        let duration = self.compute_duration(self.compute_from_date());
        if duration == 0 {
            return rust_decimal::Decimal::ZERO;
        }
        rust_decimal::Decimal::from(plan_amount_cents) / rust_decimal::Decimal::from(duration)
    }
}

// --- date helpers (ActiveSupport-equivalents) ---

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn beginning_of_month(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap()
}

fn end_of_month(date: NaiveDate) -> NaiveDate {
    let last = days_in_month(date.year(), date.month());
    NaiveDate::from_ymd_opt(date.year(), date.month(), last).unwrap()
}

fn last_day_of_month(date: NaiveDate) -> bool {
    date.day() == end_of_month(date).day()
}

/// `date - n.month` / `+ n.month` with day clamping to the target month length.
fn add_months(date: NaiveDate, delta: i32) -> NaiveDate {
    let mut m = date.month() as i32 - 1 + delta;
    let mut y = date.year();
    while m < 0 {
        m += 12;
        y -= 1;
    }
    while m >= 12 {
        m -= 12;
        y += 1;
    }
    let month = (m + 1) as u32;
    let day = date.day().min(days_in_month(y, month));
    NaiveDate::from_ymd_opt(y, month, day).unwrap()
}

/// `DatesService#build_date` — handle day 0 (→ end of previous month) and
/// anniversary days past the end of a shorter month.
fn build_date(mut year: i32, mut month: i32, mut day: i32) -> NaiveDate {
    if day == 0 {
        day = 31;
        month -= 1;
        if month == 0 {
            month = 12;
            year -= 1;
        }
    }
    let dim = days_in_month(year, month as u32) as i32;
    if dim < day {
        day = dim;
    }
    NaiveDate::from_ymd_opt(year, month as u32, day as u32).unwrap()
}

// ============================================================================
// Weekly / Quarterly / Semiannual / Yearly interval services.
//
// Port of `Subscriptions::Dates::{Weekly,Quarterly,Semiannual,Yearly}Service`
// for both billing times:
//
// * **calendar** periods are the calendar week (Mon-Sun) / quarter / half-year
//   / year;
// * **anniversary** periods start on the subscription date and repeat every
//   interval (`subscription_at + k * interval`), the day clamped to the length
//   of shorter months exactly like Lago's `build_date` (a Jan 31 quarterly
//   subscription rolls over Apr 30, Jul 31, Oct 31, Jan 31); each period ends
//   the day before the next one starts.
//
// Pay-in-arrears plans bill the period that just closed (subscription fee and
// usage charges). Pay-in-advance plans bill the subscription fee for the period
// that starts on the billing date and the usage charges of the period that just
// closed (`charges_from_date` / `charges_to_date`).
// ============================================================================

/// A subscription billing interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    Weekly,
    Monthly,
    Quarterly,
    Semiannual,
    Yearly,
}

/// Period dates for the non-monthly intervals (monthly has its own
/// [`MonthlyDates`] with full anniversary + pay-in-advance handling).
#[derive(Debug, Clone, Copy)]
pub struct PeriodDates {
    pub interval: Interval,
    pub subscription_at: NaiveDate,
    pub billing_at: NaiveDate,
    pub anniversary: bool,
    pub pay_in_advance: bool,
}

impl PeriodDates {
    /// The period that contains the billing date (the upcoming period on a
    /// rollover day).
    fn current_period(&self) -> (NaiveDate, NaiveDate) {
        period_containing(self.interval, self.anniversary, self.subscription_at, self.billing_at)
    }

    /// The last period that ended before the billing date (the one that closed
    /// on a rollover day).
    fn closing_period(&self) -> (NaiveDate, NaiveDate) {
        let (current_from, _) = self.current_period();
        period_containing(
            self.interval,
            self.anniversary,
            self.subscription_at,
            current_from - Duration::days(1),
        )
    }

    /// `#compute_from_date`: the start of the period the subscription fee
    /// covers — the closing period in arrears, the current one in advance.
    pub fn compute_from_date(&self) -> NaiveDate {
        if self.pay_in_advance {
            self.current_period().0
        } else {
            self.closing_period().0
        }
    }

    /// `#compute_to_date`: the end of the period starting at `from`.
    pub fn compute_to_date(&self, from: NaiveDate) -> NaiveDate {
        period_containing(self.interval, self.anniversary, self.subscription_at, from).1
    }

    /// `#compute_duration` — length in days of the period containing `from`.
    pub fn compute_duration(&self, from: NaiveDate) -> i64 {
        let (start, end) =
            period_containing(self.interval, self.anniversary, self.subscription_at, from);
        (end + Duration::days(1) - start).num_days()
    }

    /// Compute all boundaries: the subscription-fee period plus the usage
    /// charges period (always the closing period).
    pub fn boundaries(&self) -> Boundaries {
        let from_date = self.compute_from_date();
        let to_date = self.compute_to_date(from_date);
        let (charges_from_date, charges_to_date) = self.closing_period();
        Boundaries {
            from_date,
            to_date,
            charges_from_date,
            charges_to_date,
            charges_duration_days: self.compute_duration(charges_from_date),
        }
    }

    /// `DatesService#single_day_price`.
    pub fn single_day_price(&self, plan_amount_cents: i64) -> rust_decimal::Decimal {
        let duration = self.compute_duration(self.compute_from_date());
        if duration == 0 {
            return rust_decimal::Decimal::ZERO;
        }
        rust_decimal::Decimal::from(plan_amount_cents) / rust_decimal::Decimal::from(duration)
    }
}

/// Number of months in one period of a month-based interval.
fn months_per_period(interval: Interval) -> Option<i32> {
    match interval {
        Interval::Weekly => None,
        Interval::Monthly => Some(1),
        Interval::Quarterly => Some(3),
        Interval::Semiannual => Some(6),
        Interval::Yearly => Some(12),
    }
}

/// The start of the anniversary period `months` months after the subscription
/// date, the subscription day clamped to the target month's length.
fn anniversary_start(subscription_at: NaiveDate, months: i32) -> NaiveDate {
    let index = subscription_at.year() * 12 + subscription_at.month() as i32 - 1 + months;
    let year = index.div_euclid(12);
    let month = (index.rem_euclid(12) + 1) as u32;
    let day = subscription_at.day().min(days_in_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// The billing period `(from, to)` (both inclusive) that contains `date`, for
/// any interval and billing time. Dates before the subscription date resolve
/// to the (virtual) period preceding it, which callers clamp to the start.
pub fn period_containing(
    interval: Interval,
    anniversary: bool,
    subscription_at: NaiveDate,
    date: NaiveDate,
) -> (NaiveDate, NaiveDate) {
    if !anniversary {
        let from = match interval {
            Interval::Weekly => beginning_of_week(date),
            Interval::Monthly => beginning_of_month(date),
            Interval::Quarterly => beginning_of_quarter(date),
            Interval::Semiannual => beginning_of_half_year(date),
            Interval::Yearly => beginning_of_year(date),
        };
        let to = match interval {
            Interval::Weekly => from + Duration::days(6),
            Interval::Monthly => end_of_month(from),
            Interval::Quarterly => end_of_quarter(from),
            Interval::Semiannual => end_of_half_year(from),
            Interval::Yearly => end_of_year(from),
        };
        return (from, to);
    }
    let Some(step) = months_per_period(interval) else {
        let from = previous_weekday(date, subscription_at.weekday());
        return (from, from + Duration::days(6));
    };
    let months_since = (date.year() - subscription_at.year()) * 12 + date.month() as i32
        - subscription_at.month() as i32;
    let mut k = months_since.div_euclid(step);
    if anniversary_start(subscription_at, k * step) > date {
        k -= 1;
    }
    let from = anniversary_start(subscription_at, k * step);
    let to = anniversary_start(subscription_at, (k + 1) * step) - Duration::days(1);
    (from, to)
}

/// Whether a subscription is billed on `today` — the day its billing period
/// rolls over (a new period starts). Port of the selection in
/// `Subscriptions::BillingDateQuery` for every interval and both billing times.
pub fn due_for_billing(
    interval: Interval,
    anniversary: bool,
    subscription_at: NaiveDate,
    today: NaiveDate,
) -> bool {
    period_containing(interval, anniversary, subscription_at, today).0 == today
}

/// The billing periods of a run on `billing_at` (any interval): the
/// subscription-fee period and the usage-charges period. See [`PeriodDates`].
pub fn billing_boundaries(
    interval: Interval,
    anniversary: bool,
    subscription_at: NaiveDate,
    billing_at: NaiveDate,
    pay_in_advance: bool,
) -> Boundaries {
    PeriodDates {
        interval,
        subscription_at,
        billing_at,
        anniversary,
        pay_in_advance,
    }
    .boundaries()
}

fn beginning_of_week(date: NaiveDate) -> NaiveDate {
    // ActiveSupport weeks start on Monday.
    let dow = date.weekday().num_days_from_monday() as i64;
    date - Duration::days(dow)
}

fn previous_weekday(date: NaiveDate, target: chrono::Weekday) -> NaiveDate {
    // Most recent `target` weekday on or before `date`.
    let cur = date.weekday().num_days_from_monday() as i64;
    let tgt = target.num_days_from_monday() as i64;
    let back = (cur - tgt).rem_euclid(7);
    date - Duration::days(back)
}

fn quarter_of(month: u32) -> u32 {
    (month - 1) / 3
}

fn beginning_of_quarter(date: NaiveDate) -> NaiveDate {
    let q = quarter_of(date.month());
    NaiveDate::from_ymd_opt(date.year(), q * 3 + 1, 1).unwrap()
}

fn end_of_quarter(date: NaiveDate) -> NaiveDate {
    let q = quarter_of(date.month());
    let last_month = q * 3 + 3;
    NaiveDate::from_ymd_opt(
        date.year(),
        last_month,
        days_in_month(date.year(), last_month),
    )
    .unwrap()
}

fn beginning_of_half_year(date: NaiveDate) -> NaiveDate {
    let start_month = if date.month() <= 6 { 1 } else { 7 };
    NaiveDate::from_ymd_opt(date.year(), start_month, 1).unwrap()
}

fn end_of_half_year(date: NaiveDate) -> NaiveDate {
    if date.month() <= 6 {
        NaiveDate::from_ymd_opt(date.year(), 6, 30).unwrap()
    } else {
        NaiveDate::from_ymd_opt(date.year(), 12, 31).unwrap()
    }
}

fn beginning_of_year(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap()
}

fn end_of_year(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), 12, 31).unwrap()
}

#[cfg(test)]
mod interval_period_tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn arrears(interval: Interval, sub: NaiveDate, billing: NaiveDate) -> Boundaries {
        billing_boundaries(interval, true, sub, billing, false)
    }

    #[test]
    fn quarterly_anniversary_rolls_over_on_the_clamped_subscription_day() {
        let sub = d(2026, 1, 31);
        for due in [d(2026, 4, 30), d(2026, 7, 31), d(2026, 10, 31), d(2027, 1, 31)] {
            assert!(due_for_billing(Interval::Quarterly, true, sub, due), "{due}");
        }
        for not_due in [d(2026, 4, 29), d(2026, 5, 1), d(2026, 2, 28), d(2026, 5, 31)] {
            assert!(!due_for_billing(Interval::Quarterly, true, sub, not_due), "{not_due}");
        }
        let b = arrears(Interval::Quarterly, sub, d(2026, 4, 30));
        assert_eq!((b.from_date, b.to_date), (d(2026, 1, 31), d(2026, 4, 29)));
        assert_eq!((b.charges_from_date, b.charges_to_date), (b.from_date, b.to_date));
        assert_eq!(b.charges_duration_days, 89);
        let next = arrears(Interval::Quarterly, sub, d(2026, 7, 31));
        assert_eq!((next.from_date, next.to_date), (d(2026, 4, 30), d(2026, 7, 30)));
    }

    #[test]
    fn semiannual_anniversary_period() {
        let sub = d(2026, 3, 15);
        assert!(due_for_billing(Interval::Semiannual, true, sub, d(2026, 9, 15)));
        assert!(!due_for_billing(Interval::Semiannual, true, sub, d(2026, 6, 15)));
        let b = arrears(Interval::Semiannual, sub, d(2026, 9, 15));
        assert_eq!((b.from_date, b.to_date, b.charges_duration_days), (d(2026, 3, 15), d(2026, 9, 14), 184));
    }

    #[test]
    fn yearly_anniversary_handles_leap_day_subscriptions() {
        let sub = d(2024, 2, 29);
        assert!(due_for_billing(Interval::Yearly, true, sub, d(2025, 2, 28)));
        assert!(!due_for_billing(Interval::Yearly, true, sub, d(2025, 3, 1)));
        assert!(due_for_billing(Interval::Yearly, true, sub, d(2028, 2, 29)));
        assert!(!due_for_billing(Interval::Yearly, true, sub, d(2028, 2, 28)));
        let b = arrears(Interval::Yearly, sub, d(2025, 2, 28));
        assert_eq!((b.from_date, b.to_date, b.charges_duration_days), (d(2024, 2, 29), d(2025, 2, 27), 365));
        let b = arrears(Interval::Yearly, sub, d(2026, 2, 28));
        assert_eq!((b.from_date, b.to_date), (d(2025, 2, 28), d(2026, 2, 27)));
    }

    #[test]
    fn pay_in_advance_bills_fee_forward_and_charges_for_the_closed_period() {
        let b = billing_boundaries(Interval::Quarterly, true, d(2026, 1, 15), d(2026, 4, 15), true);
        assert_eq!((b.from_date, b.to_date), (d(2026, 4, 15), d(2026, 7, 14)));
        assert_eq!((b.charges_from_date, b.charges_to_date), (d(2026, 1, 15), d(2026, 4, 14)));
        let y = billing_boundaries(Interval::Yearly, false, d(2025, 6, 3), d(2026, 1, 1), true);
        assert_eq!((y.from_date, y.to_date), (d(2026, 1, 1), d(2026, 12, 31)));
        assert_eq!((y.charges_from_date, y.charges_to_date), (d(2025, 1, 1), d(2025, 12, 31)));
    }

    #[test]
    fn anniversary_periods_partition_time_without_gaps_or_overlaps() {
        for interval in [Interval::Weekly, Interval::Monthly, Interval::Quarterly, Interval::Semiannual, Interval::Yearly] {
            for sub in [d(2024, 1, 31), d(2024, 2, 29), d(2025, 8, 30), d(2025, 11, 1)] {
                let mut day = sub;
                let (mut from, mut to) = period_containing(interval, true, sub, day);
                assert_eq!(from, sub, "{interval:?} {sub}");
                while day < d(2029, 1, 1) {
                    day += Duration::days(1);
                    let (f, t) = period_containing(interval, true, sub, day);
                    if day > to {
                        assert_eq!(f, to + Duration::days(1), "{interval:?} {sub} {day}");
                        assert!(due_for_billing(interval, true, sub, day));
                        from = f;
                        to = t;
                    } else {
                        assert_eq!((f, t), (from, to), "{interval:?} {sub} {day}");
                        assert!(!due_for_billing(interval, true, sub, day));
                    }
                }
            }
        }
    }

    /// The generic period walk agrees with the ported `MonthlyService` on
    /// every monthly rollover (anniversary + calendar, arrears + advance).
    #[test]
    fn monthly_matches_the_monthly_service() {
        let mut sub = d(2024, 1, 1);
        while sub < d(2025, 3, 1) {
            let mut billing = sub + Duration::days(1);
            while billing < sub + Duration::days(800) {
                for anniversary in [true, false] {
                    if !due_for_billing(Interval::Monthly, anniversary, sub, billing) {
                        continue;
                    }
                    for pay_in_advance in [false, true] {
                        let monthly = MonthlyDates {
                            subscription_at: sub,
                            billing_at: billing,
                            anniversary,
                            pay_in_advance,
                            current_usage: false,
                        }
                        .boundaries();
                        let generic = billing_boundaries(Interval::Monthly, anniversary, sub, billing, pay_in_advance);
                        assert_eq!(
                            (generic.from_date, generic.to_date, generic.charges_from_date, generic.charges_to_date),
                            (monthly.from_date, monthly.to_date, monthly.charges_from_date, monthly.charges_to_date),
                            "sub {sub} billing {billing} anniversary {anniversary} advance {pay_in_advance}"
                        );
                    }
                }
                billing += Duration::days(1);
            }
            sub += Duration::days(3);
        }
    }
}

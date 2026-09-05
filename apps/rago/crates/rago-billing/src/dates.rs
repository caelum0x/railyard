//! Subscription billing-period boundaries.
//!
//! Faithful port of `Subscriptions::DatesService` + `Dates::MonthlyService`
//! for the monthly interval (calendar and anniversary billing), covering the
//! standard non-terminated period plus proration inputs (`single_day_price`,
//! `charges_duration`). Dates are computed at day granularity; the Ruby
//! customer-timezone shift is applied by callers when converting a date to a
//! UTC datetime (noted as a simplification — the day-level period math matches).
//!
//! The weekly/yearly/quarterly/semiannual interval services follow the same
//! shape and are catalogued in the port map.

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
// Faithful port of `Subscriptions::Dates::{Weekly,Quarterly,Semiannual,Yearly}
// Service`. The **calendar** billing path is implemented for all four; the
// **anniversary** path is implemented for weekly (whose period is always
// `from + 6 days`). Anniversary billing for quarterly/semiannual/yearly (the
// `previous_anniversary_day` walk over the billing months, with an anniversary
// end-of-period distinct from the calendar end) is catalogued in the port map
// as a follow-up — calendar billing, the common case, is complete for every
// interval, and monthly anniversary is covered by [`MonthlyDates`].
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
    fn calendar(&self) -> bool {
        !self.anniversary
    }

    /// `#base_date` (billing context): one interval before the billing date.
    fn base_date(&self) -> NaiveDate {
        match self.interval {
            Interval::Weekly => self.billing_at - Duration::weeks(1),
            Interval::Quarterly => add_months(self.billing_at, -3),
            Interval::Semiannual => add_months(self.billing_at, -6),
            Interval::Yearly => add_months(self.billing_at, -12),
            Interval::Monthly => add_months(self.billing_at, -1),
        }
    }

    fn beginning_of_period(&self, date: NaiveDate) -> NaiveDate {
        match self.interval {
            Interval::Weekly => beginning_of_week(date),
            Interval::Quarterly => beginning_of_quarter(date),
            Interval::Semiannual => beginning_of_half_year(date),
            Interval::Yearly => beginning_of_year(date),
            Interval::Monthly => beginning_of_month(date),
        }
    }

    fn end_of_period(&self, from: NaiveDate) -> NaiveDate {
        match self.interval {
            Interval::Weekly => from + Duration::days(6),
            Interval::Quarterly => end_of_quarter(from),
            Interval::Semiannual => end_of_half_year(from),
            Interval::Yearly => end_of_year(from),
            Interval::Monthly => end_of_month(from),
        }
    }

    /// `#compute_from_date` (pay-in-arrears billing).
    pub fn compute_from_date(&self) -> NaiveDate {
        if self.calendar() {
            return self.beginning_of_period(self.base_date());
        }
        // Anniversary: weekly only (see module note); others use the calendar
        // beginning as a documented approximation until their anniversary walk
        // is ported.
        match self.interval {
            Interval::Weekly => previous_weekday(self.base_date(), self.subscription_at.weekday()),
            _ => self.beginning_of_period(self.base_date()),
        }
    }

    /// `#compute_to_date`.
    pub fn compute_to_date(&self, from: NaiveDate) -> NaiveDate {
        self.end_of_period(from)
    }

    /// `#compute_duration` — length of the period in days.
    pub fn compute_duration(&self, from: NaiveDate) -> i64 {
        if self.interval == Interval::Weekly {
            return 7;
        }
        let period_start = self.beginning_of_period_for_duration(from);
        let period_end = self.end_of_period(period_start);
        (period_end + Duration::days(1) - period_start).num_days()
    }

    fn beginning_of_period_for_duration(&self, from: NaiveDate) -> NaiveDate {
        if !self.calendar() && self.interval == Interval::Weekly {
            previous_weekday(from, self.subscription_at.weekday())
        } else {
            self.beginning_of_period(from)
        }
    }

    /// Compute all boundaries for the current period.
    pub fn boundaries(&self) -> Boundaries {
        let from_date = self.compute_from_date();
        let to_date = self.compute_to_date(from_date);
        Boundaries {
            from_date,
            to_date,
            charges_from_date: from_date,
            charges_to_date: to_date,
            charges_duration_days: self.compute_duration(from_date),
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

/// Whether a subscription is billed on `today` — the day its billing period
/// rolls over. Port of the selection in `Subscriptions::BillingDateQuery` /
/// `OrganizationBillingService#billable_subscriptions` for the calendar case of
/// every interval and monthly/weekly anniversary (quarterly/semiannual/yearly
/// anniversary walks are follow-ups; calendar billing is complete).
pub fn due_for_billing(
    interval: Interval,
    anniversary: bool,
    subscription_at: NaiveDate,
    today: NaiveDate,
) -> bool {
    if !anniversary {
        // Calendar: billed on the first day of the new period.
        return match interval {
            Interval::Weekly => today.weekday() == chrono::Weekday::Mon,
            Interval::Monthly => today.day() == 1,
            Interval::Quarterly => today.day() == 1 && matches!(today.month(), 1 | 4 | 7 | 10),
            Interval::Semiannual => today.day() == 1 && matches!(today.month(), 1 | 7),
            Interval::Yearly => today.day() == 1 && today.month() == 1,
        };
    }
    // Anniversary.
    match interval {
        Interval::Weekly => today.weekday() == subscription_at.weekday(),
        Interval::Monthly => {
            let day = subscription_at
                .day()
                .min(days_in_month(today.year(), today.month()));
            today.day() == day
        }
        // Quarterly/semiannual/yearly anniversary: follow-up.
        _ => false,
    }
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

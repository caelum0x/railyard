//! Free-trial billing semantics.
//!
//! Faithful port of the trial handling inside `Fees::SubscriptionService`
//! (`first_subscription_amount` / `terminated_amount` / `upgraded_amount` /
//! `full_period_amount`) plus `Subscription#in_trial_period?` and
//! `Subscription#trial_end_datetime`.
//!
//! Lago never bills usage or the flat fee *during* a free trial; it bills from
//! the trial end. The three rules every `*_amount` method shares are:
//!
//!   * `return 0 if trial_end_datetime >= to_datetime` — the trial covers the
//!     whole period, so nothing is billed.
//!   * `from = trial_end_datetime if from_datetime < trial_end < to_datetime` —
//!     the trial ends mid-period, so billing starts at the trial end.
//!   * otherwise the trial ended before the period, so the normal amount applies.
//!
//! Day counts use `Utils::Datetime.date_diff_with_timezone`, which is
//! `ceil((to - from) / 1.day)` (see [`date_diff_ceil`]). This is the datetime
//! twin of [`crate::subscriptions::date_diff`] — for a midnight-to-end-of-day
//! period the two agree exactly.

use chrono::{Duration, NaiveDateTime};

use crate::subscription_fee::Proration;

/// `Subscription#trial_end_datetime` — `initial_started_at + trial_period.days`.
/// `trial_period` is a (possibly fractional) number of days.
pub fn trial_end_datetime(
    initial_started_at: NaiveDateTime,
    trial_period_days: f64,
) -> NaiveDateTime {
    let seconds = (trial_period_days * 86_400.0).round() as i64;
    initial_started_at + Duration::seconds(seconds)
}

/// `Subscription#in_trial_period?` — a subscription is in trial when it has not
/// already been flagged as trial-ended, it has actually started, and the trial
/// end lies in the future relative to `now`.
pub fn in_trial_period(
    trial_ended_at: Option<NaiveDateTime>,
    initial_started_at: NaiveDateTime,
    trial_end: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> bool {
    if trial_ended_at.is_some() {
        return false;
    }
    if initial_started_at > now {
        return false;
    }
    matches!(trial_end, Some(te) if te > now)
}

/// `Utils::Datetime.date_diff_with_timezone` with a UTC (zero-offset) clock:
/// `ceil((to - from) / 1.day)`, with Lago's "+1 second if `to` is at the
/// beginning of a day so we never miss a day" guard.
pub fn date_diff_ceil(from: NaiveDateTime, to: NaiveDateTime) -> i64 {
    // "to += 1.second if to is exactly at beginning_of_day"
    let to = if to.time() == chrono::NaiveTime::MIN {
        to + Duration::seconds(1)
    } else {
        to
    };
    let micros = (to - from)
        .num_microseconds()
        .unwrap_or_else(|| (to - from).num_seconds() * 1_000_000);
    let day_micros = 86_400i64 * 1_000_000;
    // ceil division for positive spans.
    if micros <= 0 {
        // Negative or zero span: ceil toward zero as Ruby's ceil would.
        -((-micros) / day_micros)
    } else {
        (micros + day_micros - 1) / day_micros
    }
}

/// The datetime billing should start from once the trial is taken into account,
/// or `None` when the trial covers the whole period (bill nothing).
///
/// Mirrors the shared trial branch of `Fees::SubscriptionService`:
/// * no trial → bill from `from_datetime`.
/// * `trial_end >= to_datetime` → `None` (amount 0).
/// * `from_datetime < trial_end < to_datetime` → bill from `trial_end`.
/// * `trial_end <= from_datetime` → bill from `from_datetime`.
pub fn effective_billing_start(
    from_datetime: NaiveDateTime,
    to_datetime: NaiveDateTime,
    trial_end: Option<NaiveDateTime>,
) -> Option<NaiveDateTime> {
    let Some(te) = trial_end else {
        return Some(from_datetime);
    };
    if te >= to_datetime {
        return None;
    }
    if te > from_datetime && te < to_datetime {
        Some(te)
    } else {
        Some(from_datetime)
    }
}

/// Trial-aware [`Proration`] for a **prorated** period (first period,
/// termination, upgrade): the number of days billed is
/// `date_diff_ceil(effective_from, to)`, unless the trial covers the whole
/// period (→ [`Proration::Zero`]).
pub fn prorated_proration_with_trial(
    from_datetime: NaiveDateTime,
    to_datetime: NaiveDateTime,
    trial_end: Option<NaiveDateTime>,
) -> Proration {
    match effective_billing_start(from_datetime, to_datetime, trial_end) {
        None => Proration::Zero,
        Some(f) => Proration::Prorated {
            days_to_bill: date_diff_ceil(f, to_datetime),
        },
    }
}

/// Trial-aware [`Proration`] for a **full** period (`full_period_amount`):
/// normally the whole plan amount, but if the trial ends mid-period Lago
/// prorates the days from the trial end to the period end, and if the trial
/// covers the whole period it bills nothing.
pub fn full_period_proration_with_trial(
    from_datetime: NaiveDateTime,
    to_datetime: NaiveDateTime,
    trial_end: Option<NaiveDateTime>,
) -> Proration {
    let Some(te) = trial_end else {
        return Proration::Full;
    };
    if te >= to_datetime {
        return Proration::Zero;
    }
    if te > from_datetime && te < to_datetime {
        Proration::Prorated {
            days_to_bill: date_diff_ceil(te, to_datetime),
        }
    } else {
        Proration::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, ss)
            .unwrap()
    }
    fn eod(y: i32, m: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap()
    }

    #[test]
    fn trial_end_is_started_plus_days() {
        let start = dt(2022, 6, 9, 10, 0, 0);
        assert_eq!(trial_end_datetime(start, 30.0), dt(2022, 7, 9, 10, 0, 0));
    }

    #[test]
    fn date_diff_ceil_full_month_period() {
        // Beginning of period to end-of-day of the last day → inclusive day count.
        assert_eq!(
            date_diff_ceil(dt(2022, 7, 1, 0, 0, 0), eod(2022, 7, 31)),
            31
        );
        // Mirrors crate::subscriptions::date_diff((07-01)->(07-31)) = 31.
    }

    #[test]
    fn date_diff_ceil_ceils_partial_day() {
        // trial_end mid-day 07-09 10:00 to 07-31 23:59:59 → 23 (07-09..07-31 inclusive).
        assert_eq!(
            date_diff_ceil(dt(2022, 7, 9, 10, 0, 0), eod(2022, 7, 31)),
            23
        );
    }

    #[test]
    fn trial_covers_whole_period_bills_zero() {
        // trial_end after period end.
        let p = full_period_proration_with_trial(
            dt(2022, 7, 1, 0, 0, 0),
            eod(2022, 7, 31),
            Some(dt(2022, 8, 15, 0, 0, 0)),
        );
        assert!(matches!(p, Proration::Zero));

        let p2 = prorated_proration_with_trial(
            dt(2022, 7, 1, 0, 0, 0),
            eod(2022, 7, 31),
            Some(eod(2022, 7, 31)), // exactly at to → >= to → zero
        );
        assert!(matches!(p2, Proration::Zero));
    }

    #[test]
    fn trial_ends_mid_period_prorates_from_trial_end() {
        // Full period with trial ending 07-09 10:00 → bill 23 days.
        let p = full_period_proration_with_trial(
            dt(2022, 7, 1, 0, 0, 0),
            eod(2022, 7, 31),
            Some(dt(2022, 7, 9, 10, 0, 0)),
        );
        assert!(matches!(p, Proration::Prorated { days_to_bill: 23 }));
    }

    #[test]
    fn trial_ended_before_period_bills_normally() {
        let p = full_period_proration_with_trial(
            dt(2022, 7, 1, 0, 0, 0),
            eod(2022, 7, 31),
            Some(dt(2022, 6, 15, 0, 0, 0)),
        );
        assert!(matches!(p, Proration::Full));

        let p2 = prorated_proration_with_trial(
            dt(2022, 7, 10, 0, 0, 0),
            eod(2022, 7, 31),
            Some(dt(2022, 6, 15, 0, 0, 0)),
        );
        // No trial impact → 22 days (07-10..07-31).
        assert!(matches!(p2, Proration::Prorated { days_to_bill: 22 }));
    }

    #[test]
    fn in_trial_period_rules() {
        let start = dt(2022, 6, 9, 0, 0, 0);
        let te = dt(2022, 7, 9, 0, 0, 0);
        // In trial: now before trial end.
        assert!(in_trial_period(
            None,
            start,
            Some(te),
            dt(2022, 6, 20, 0, 0, 0)
        ));
        // Not in trial: already flagged ended.
        assert!(!in_trial_period(
            Some(dt(2022, 6, 20, 0, 0, 0)),
            start,
            Some(te),
            dt(2022, 6, 21, 0, 0, 0)
        ));
        // Not in trial: now after trial end.
        assert!(!in_trial_period(
            None,
            start,
            Some(te),
            dt(2022, 7, 10, 0, 0, 0)
        ));
        // Not in trial: no trial configured.
        assert!(!in_trial_period(
            None,
            start,
            None,
            dt(2022, 6, 20, 0, 0, 0)
        ));
        // Not in trial: subscription starts in the future.
        assert!(!in_trial_period(
            None,
            dt(2022, 8, 1, 0, 0, 0),
            Some(dt(2022, 9, 1, 0, 0, 0)),
            dt(2022, 7, 1, 0, 0, 0)
        ));
    }
}

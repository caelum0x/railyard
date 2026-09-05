//! Usage thresholds — threshold-based (progressive) invoicing triggers.
//!
//! Faithful port of `LifetimeUsages::UsageThresholds::CheckService` (which
//! thresholds a subscription has crossed and therefore must be progressively
//! billed for) plus the `UsageThreshold` model's `recurring` semantics.
//!
//! A plan (or subscription) carries a set of **fixed** thresholds (bill once
//! when lifetime usage first crosses each amount) and at most one **recurring**
//! threshold (bill every time usage advances another `amount_cents` past the
//! largest fixed threshold). [`check_thresholds`] returns exactly the thresholds
//! that were passed on this check, in Lago's order (fixed ascending, then the
//! recurring one appended), which the progressive-billing invoice is then
//! generated for.

use rust_decimal::Decimal;

/// `UsageThreshold` — a lifetime-usage amount that triggers a progressive
/// invoice when crossed. `recurring` thresholds fire repeatedly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageThreshold {
    pub id: uuid::Uuid,
    pub amount_cents: i64,
    pub recurring: bool,
}

impl UsageThreshold {
    pub fn fixed(amount_cents: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            amount_cents,
            recurring: false,
        }
    }
    pub fn recurring(amount_cents: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            amount_cents,
            recurring: true,
        }
    }
}

/// The lifetime-usage snapshot consumed by the check
/// (`LifetimeUsage#{historical,invoiced,current}_usage_amount_cents`).
#[derive(Debug, Clone, Copy, Default)]
pub struct LifetimeUsage {
    pub historical_usage_amount_cents: i64,
    pub invoiced_usage_amount_cents: i64,
    pub current_usage_amount_cents: i64,
}

/// `LifetimeUsages::UsageThresholds::CheckService#call` — the thresholds passed
/// on this check, given the already progressively-billed amount.
///
/// Returns the thresholds in Lago's exact order: crossed fixed thresholds in
/// ascending amount order, then the recurring threshold appended once if it was
/// passed. An empty result means nothing new to bill.
pub fn check_thresholds(
    thresholds: &[UsageThreshold],
    lifetime_usage: LifetimeUsage,
    progressive_billed_amount: i64,
) -> Vec<UsageThreshold> {
    let mut passed: Vec<UsageThreshold> = Vec::new();
    if thresholds.is_empty() {
        return passed;
    }

    // fixed_thresholds ordered by amount_cents asc; the single recurring one.
    let mut fixed: Vec<&UsageThreshold> = thresholds.iter().filter(|t| !t.recurring).collect();
    fixed.sort_by_key(|t| t.amount_cents);
    let recurring = thresholds.iter().find(|t| t.recurring);

    // actual_current_usage = current - progressive_billed; below zero → nothing.
    let actual_current_usage =
        lifetime_usage.current_usage_amount_cents - progressive_billed_amount;
    if actual_current_usage < 0 {
        return passed;
    }
    let invoiced_usage = lifetime_usage.historical_usage_amount_cents
        + lifetime_usage.invoiced_usage_amount_cents
        + progressive_billed_amount;

    let largest_threshold_amount = fixed.iter().map(|t| t.amount_cents).max().unwrap_or(0);
    let total_usage = invoiced_usage + actual_current_usage;

    if invoiced_usage < largest_threshold_amount {
        // Below some fixed thresholds: keep those newly crossed by total_usage.
        for t in &fixed {
            if t.amount_cents > invoiced_usage && t.amount_cents <= total_usage {
                passed.push((*t).clone());
            }
        }
        if let Some(rec) = recurring {
            if total_usage - largest_threshold_amount >= rec.amount_cents {
                passed.push(rec.clone());
            }
        }
    } else if let Some(rec) = recurring {
        // Above all fixed thresholds: recurring fires on each new multiple.
        let recurring_remainder = invoiced_usage % rec.amount_cents;
        if actual_current_usage + recurring_remainder >= rec.amount_cents {
            passed.push(rec.clone());
        }
    }

    passed
}

/// `LifetimeUsage#total_amount_cents`.
pub fn total_amount_cents(lu: LifetimeUsage) -> i64 {
    lu.historical_usage_amount_cents
        + lu.invoiced_usage_amount_cents
        + lu.current_usage_amount_cents
}

/// `LifetimeUsages::FindLastAndNextThresholdsService` completion ratio for the
/// next unpassed fixed threshold, exposed for usage displays:
/// `(total_usage - last_passed) / (next - last_passed)`.
pub fn next_threshold_completion_ratio(
    thresholds: &[UsageThreshold],
    total_usage_cents: i64,
) -> Option<Decimal> {
    let mut fixed: Vec<&UsageThreshold> = thresholds.iter().filter(|t| !t.recurring).collect();
    fixed.sort_by_key(|t| t.amount_cents);
    let last_passed = fixed
        .iter()
        .filter(|t| t.amount_cents <= total_usage_cents)
        .map(|t| t.amount_cents)
        .max()
        .unwrap_or(0);
    let next = fixed.iter().find(|t| t.amount_cents > total_usage_cents)?;
    let denom = next.amount_cents - last_passed;
    if denom <= 0 {
        return None;
    }
    Some(Decimal::from(total_usage_cents - last_passed) / Decimal::from(denom))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amounts(passed: &[UsageThreshold]) -> Vec<i64> {
        passed.iter().map(|t| t.amount_cents).collect()
    }

    /// Runs a `[invoiced, current] => [expected amounts]` row from Lago's
    /// `usage_thresholds/check_service_spec.rb` (historical = 0, no prior
    /// progressive-billed amount).
    fn run(thresholds: &[UsageThreshold], invoiced: i64, current: i64) -> Vec<i64> {
        let lu = LifetimeUsage {
            historical_usage_amount_cents: 0,
            invoiced_usage_amount_cents: invoiced,
            current_usage_amount_cents: current,
        };
        amounts(&check_thresholds(thresholds, lu, 0))
    }

    // ---- Lago spec: usage_thresholds/check_service_spec.rb ----

    #[test]
    fn multiple_fixed_thresholds() {
        let t: Vec<UsageThreshold> = [10, 20, 31, 40]
            .iter()
            .map(|a| UsageThreshold::fixed(*a))
            .collect();
        assert_eq!(run(&t, 0, 7), Vec::<i64>::new());
        assert_eq!(run(&t, 0, 10), vec![10]);
        assert_eq!(run(&t, 0, 31), vec![10, 20, 31]);
        assert_eq!(run(&t, 9, 2), vec![10]);
        assert_eq!(run(&t, 9, 20), vec![10, 20]);
        assert_eq!(run(&t, 9, 31), vec![10, 20, 31, 40]);
        assert_eq!(run(&t, 11, 1), Vec::<i64>::new());
        assert_eq!(run(&t, 11, 10), vec![20]);
        assert_eq!(run(&t, 21, 20), vec![31, 40]);
        assert_eq!(run(&t, 40, 2), Vec::<i64>::new());
        assert_eq!(run(&t, 50, 0), Vec::<i64>::new());
    }

    #[test]
    fn no_fixed_recurring_10() {
        let t = vec![UsageThreshold::recurring(10)];
        assert_eq!(run(&t, 0, 7), Vec::<i64>::new());
        assert_eq!(run(&t, 0, 10), vec![10]);
        assert_eq!(run(&t, 9, 2), vec![10]);
        assert_eq!(run(&t, 11, 1), Vec::<i64>::new());
        assert_eq!(run(&t, 11, 8), Vec::<i64>::new());
        assert_eq!(run(&t, 11, 9), vec![10]);
        assert_eq!(run(&t, 11, 10), vec![10]);
        assert_eq!(run(&t, 11, 20), vec![10]);
        assert_eq!(run(&t, 202, 7), Vec::<i64>::new());
        assert_eq!(run(&t, 202, 8), vec![10]);
    }

    #[test]
    fn one_fixed_10_recurring_5() {
        let t = vec![UsageThreshold::fixed(10), UsageThreshold::recurring(5)];
        assert_eq!(run(&t, 0, 7), Vec::<i64>::new());
        assert_eq!(run(&t, 0, 10), vec![10]);
        assert_eq!(run(&t, 0, 15), vec![10, 5]);
        assert_eq!(run(&t, 0, 20), vec![10, 5]);
        assert_eq!(run(&t, 9, 2), vec![10]);
        assert_eq!(run(&t, 9, 6), vec![10, 5]);
        assert_eq!(run(&t, 9, 20), vec![10, 5]);
        assert_eq!(run(&t, 11, 3), Vec::<i64>::new());
        assert_eq!(run(&t, 11, 4), vec![5]);
        assert_eq!(run(&t, 11, 20), vec![5]);
    }

    #[test]
    fn multiple_fixed_and_recurring_5() {
        let mut t: Vec<UsageThreshold> = [10, 20, 31, 40]
            .iter()
            .map(|a| UsageThreshold::fixed(*a))
            .collect();
        t.push(UsageThreshold::recurring(5));
        assert_eq!(run(&t, 0, 7), Vec::<i64>::new());
        assert_eq!(run(&t, 0, 10), vec![10]);
        assert_eq!(run(&t, 0, 31), vec![10, 20, 31]);
        assert_eq!(run(&t, 0, 44), vec![10, 20, 31, 40]);
        assert_eq!(run(&t, 0, 45), vec![10, 20, 31, 40, 5]);
        assert_eq!(run(&t, 9, 2), vec![10]);
        assert_eq!(run(&t, 9, 20), vec![10, 20]);
        assert_eq!(run(&t, 9, 31), vec![10, 20, 31, 40]);
        assert_eq!(run(&t, 9, 37), vec![10, 20, 31, 40, 5]);
        assert_eq!(run(&t, 11, 1), Vec::<i64>::new());
        assert_eq!(run(&t, 11, 10), vec![20]);
        assert_eq!(run(&t, 21, 20), vec![31, 40]);
        assert_eq!(run(&t, 21, 24), vec![31, 40, 5]);
        assert_eq!(run(&t, 40, 2), Vec::<i64>::new());
        assert_eq!(run(&t, 40, 5), vec![5]);
        assert_eq!(run(&t, 41, 4), vec![5]);
        assert_eq!(run(&t, 49, 1), vec![5]);
        assert_eq!(run(&t, 50, 0), Vec::<i64>::new());
        assert_eq!(run(&t, 50, 5), vec![5]);
    }

    #[test]
    fn no_thresholds_never_passes() {
        assert_eq!(run(&[], 0, 100), Vec::<i64>::new());
    }

    #[test]
    fn historical_usage_counts_as_invoiced() {
        // historical 11 + current 1 with a single fixed 10 → already above 10.
        let t = vec![UsageThreshold::fixed(10)];
        let lu = LifetimeUsage {
            historical_usage_amount_cents: 11,
            invoiced_usage_amount_cents: 0,
            current_usage_amount_cents: 1,
        };
        assert_eq!(amounts(&check_thresholds(&t, lu, 0)), Vec::<i64>::new());
    }

    #[test]
    fn progressive_billed_amount_is_subtracted() {
        // current 20, already progressively billed 15, recurring 10, invoiced 0.
        // actual_current = 5; invoiced_usage = 15; total = 20; largest = 0.
        // invoiced(15) >= largest(0) → recurring branch: remainder = 15 % 10 = 5;
        // actual(5) + 5 = 10 >= 10 → passes recurring.
        let t = vec![UsageThreshold::recurring(10)];
        let lu = LifetimeUsage {
            historical_usage_amount_cents: 0,
            invoiced_usage_amount_cents: 0,
            current_usage_amount_cents: 20,
        };
        assert_eq!(amounts(&check_thresholds(&t, lu, 15)), vec![10]);
    }
}

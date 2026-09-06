//! Dunning campaign evaluation — the PURE decision core of automated
//! failed-payment recovery.
//!
//! Faithful port of Lago's
//! `DunningCampaigns::BulkProcessService::CustomerDunningEvaluator` and the
//! amount math in `PaymentRequests::CreateService`. Everything here is a pure
//! function of its inputs (no I/O), so the money-critical decisions —
//! which currencies fire, how attempts advance, when a campaign finishes, and
//! the net amount to collect — are exhaustively unit-testable. The `rago-jobs`
//! sweep supplies the DB-loaded inputs and performs the side effects.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

/// A campaign's dunning policy (`dunning_campaigns` row).
#[derive(Debug, Clone, Copy)]
pub struct DunningPolicy {
    /// Maximum attempts per currency before that currency is exhausted.
    pub max_attempts: i32,
    /// Minimum days that must elapse between two attempts for a customer.
    pub days_between_attempts: i32,
}

/// A per-currency threshold (`dunning_campaign_thresholds` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Threshold {
    pub currency: String,
    pub amount_cents: i64,
}

/// The evaluator's decision for one customer against one campaign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DunningDecision {
    /// Thresholds that fired this run — one per currency, each becoming a
    /// payment request. Empty ⇒ nothing to do this run.
    pub fired: Vec<Threshold>,
    /// The customer's `dunning_currency_attempts` map after this run.
    pub new_attempts: BTreeMap<String, i64>,
    /// When set, the customer's `last_dunning_campaign_attempt_at` advances to
    /// this instant (only when at least one threshold fired).
    pub attempt_at: Option<DateTime<Utc>>,
    /// True when every dunned currency has reached `max_attempts` — emit the
    /// `dunning_campaign.finished` webhook exactly once on the transition.
    pub finished: bool,
}

impl DunningDecision {
    /// A no-op decision: nothing fired, attempts unchanged.
    fn noop(attempts: &BTreeMap<String, i64>) -> Self {
        Self {
            fired: Vec::new(),
            new_attempts: attempts.clone(),
            attempt_at: None,
            finished: false,
        }
    }
}

/// Whether enough days have elapsed since the customer's last attempt.
///
/// Ports `days_between_attempts_satisfied?`: with no prior attempt it is always
/// satisfied; otherwise `now` must be strictly past
/// `last + days_between_attempts` (a same-instant re-run is therefore gated,
/// which is what makes a double sweep idempotent).
fn days_between_satisfied(
    last_attempt: Option<DateTime<Utc>>,
    days_between_attempts: i32,
    now: DateTime<Utc>,
) -> bool {
    match last_attempt {
        None => true,
        Some(last) => now > last + Duration::days(days_between_attempts.max(0) as i64),
    }
}

/// Evaluate one customer against one campaign.
///
/// * `thresholds` — every threshold configured for the campaign.
/// * `attempts` — the customer's current per-currency attempt counts.
/// * `overdue_by_currency` — the customer's overdue balance per currency
///   (`Customer#overdue_balances`: raw `SUM(total_amount_cents)`, NOT net of
///   credits — the net is only applied to the payment-request amount).
///
/// Faithful to `CustomerDunningEvaluator#call`:
/// 1. gate on `days_between_attempts_satisfied?`;
/// 2. for each currency, skip if already at `max_attempts`, else select the
///    highest threshold whose `amount_cents <= overdue`;
/// 3. if none fire, no-op; otherwise increment the fired currencies' attempts
///    and stamp `last_dunning_campaign_attempt_at`;
/// 4. mark `finished` when every currency in the (updated) attempts map has
///    reached `max_attempts`.
pub fn evaluate_customer(
    policy: DunningPolicy,
    thresholds: &[Threshold],
    attempts: &BTreeMap<String, i64>,
    last_attempt: Option<DateTime<Utc>>,
    overdue_by_currency: &BTreeMap<String, i64>,
    now: DateTime<Utc>,
) -> DunningDecision {
    if !days_between_satisfied(last_attempt, policy.days_between_attempts, now) {
        return DunningDecision::noop(attempts);
    }

    let max = policy.max_attempts.max(0) as i64;
    let mut fired: Vec<Threshold> = Vec::new();
    for (currency, &overdue) in overdue_by_currency {
        // Skip currencies that have already exhausted their attempts.
        if attempts.get(currency).copied().unwrap_or(0) >= max {
            continue;
        }
        // Highest threshold at-or-below the overdue amount for this currency.
        if let Some(t) = thresholds
            .iter()
            .filter(|t| &t.currency == currency && t.amount_cents <= overdue)
            .max_by_key(|t| t.amount_cents)
        {
            fired.push(t.clone());
        }
    }

    if fired.is_empty() {
        return DunningDecision::noop(attempts);
    }

    let mut new_attempts = attempts.clone();
    for t in &fired {
        *new_attempts.entry(t.currency.clone()).or_insert(0) += 1;
    }

    let finished = !new_attempts.is_empty() && new_attempts.values().all(|&count| count >= max);

    DunningDecision {
        fired,
        new_attempts,
        attempt_at: Some(now),
        finished,
    }
}

// ---- payment-request amount + preconditions --------------------------------

/// An overdue invoice as seen by the payment-request builder.
#[derive(Debug, Clone)]
pub struct OverdueInvoice {
    pub id: Uuid,
    pub currency: String,
    pub total_amount_cents: i64,
    /// Amount already paid on the invoice (Lago `total_paid_amount_cents`).
    pub total_paid_amount_cents: i64,
    /// Finalized credit-note offset against this invoice (Lago
    /// `CreditNote.finalized.sum(:offset_amount_cents)`).
    pub credit_note_offset_cents: i64,
    pub payment_overdue: bool,
}

/// Why a payment request cannot be built — ports `check_preconditions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentRequestError {
    /// No invoices supplied.
    NoInvoices,
    /// At least one supplied invoice is not overdue.
    InvoicesNotOverdue,
    /// The supplied invoices span more than one currency.
    DifferentCurrencies,
}

/// A validated payment-request plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRequestPlan {
    pub currency: String,
    /// Net amount to collect (never negative).
    pub amount_cents: i64,
    pub invoice_ids: Vec<Uuid>,
}

/// Validate and price a payment request over a set of overdue invoices.
///
/// Ports `PaymentRequests::CreateService`:
/// `total = Σ(total_amount_cents − total_paid_amount_cents) − Σ credit-note offset`,
/// after the `check_preconditions` guards (non-empty, all overdue, single
/// currency). The net is floored at zero — a fully-credited batch never yields
/// a negative charge.
pub fn build_payment_request(
    invoices: &[OverdueInvoice],
) -> Result<PaymentRequestPlan, PaymentRequestError> {
    if invoices.is_empty() {
        return Err(PaymentRequestError::NoInvoices);
    }
    if invoices.iter().any(|i| !i.payment_overdue) {
        return Err(PaymentRequestError::InvoicesNotOverdue);
    }
    let currency = invoices[0].currency.clone();
    if invoices.iter().any(|i| i.currency != currency) {
        return Err(PaymentRequestError::DifferentCurrencies);
    }

    let gross: i64 = invoices
        .iter()
        .map(|i| i.total_amount_cents - i.total_paid_amount_cents)
        .sum();
    let offset: i64 = invoices.iter().map(|i| i.credit_note_offset_cents).sum();
    let amount_cents = (gross - offset).max(0);

    Ok(PaymentRequestPlan {
        currency,
        amount_cents,
        invoice_ids: invoices.iter().map(|i| i.id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn policy(max: i32, days: i32) -> DunningPolicy {
        DunningPolicy {
            max_attempts: max,
            days_between_attempts: days,
        }
    }

    fn threshold(currency: &str, amount: i64) -> Threshold {
        Threshold {
            currency: currency.into(),
            amount_cents: amount,
        }
    }

    fn balances(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
        pairs.iter().map(|(c, a)| (c.to_string(), *a)).collect()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn fires_highest_threshold_at_or_below_overdue() {
        let thresholds = vec![
            threshold("USD", 1000),
            threshold("USD", 5000),
            threshold("USD", 20000),
        ];
        // Overdue 7000 → highest threshold ≤ 7000 is 5000.
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &BTreeMap::new(),
            None,
            &balances(&[("USD", 7000)]),
            now(),
        );
        assert_eq!(d.fired, vec![threshold("USD", 5000)]);
        assert_eq!(d.new_attempts.get("USD"), Some(&1));
        assert_eq!(d.attempt_at, Some(now()));
        assert!(!d.finished);
    }

    #[test]
    fn no_fire_when_overdue_below_smallest_threshold() {
        let thresholds = vec![threshold("USD", 5000)];
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &BTreeMap::new(),
            None,
            &balances(&[("USD", 4999)]),
            now(),
        );
        assert!(d.fired.is_empty());
        assert!(d.attempt_at.is_none());
        assert!(d.new_attempts.is_empty());
    }

    #[test]
    fn days_between_gate_blocks_a_second_attempt_in_window() {
        let thresholds = vec![threshold("USD", 5000)];
        // Last attempt was 3 days ago, window is 7 days → gated.
        let last = now() - Duration::days(3);
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &balances(&[("USD", 1)]),
            Some(last),
            &balances(&[("USD", 7000)]),
            now(),
        );
        assert!(d.fired.is_empty(), "within window → no new attempt");
        assert!(d.attempt_at.is_none());
        assert_eq!(d.new_attempts.get("USD"), Some(&1), "attempts unchanged");
    }

    #[test]
    fn days_between_gate_allows_attempt_after_window() {
        let thresholds = vec![threshold("USD", 5000)];
        let last = now() - Duration::days(8);
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &balances(&[("USD", 1)]),
            Some(last),
            &balances(&[("USD", 7000)]),
            now(),
        );
        assert_eq!(d.fired, vec![threshold("USD", 5000)]);
        assert_eq!(d.new_attempts.get("USD"), Some(&2));
    }

    #[test]
    fn skips_currency_at_max_attempts() {
        let thresholds = vec![threshold("USD", 5000), threshold("EUR", 5000)];
        // USD already at max (3); EUR fresh → only EUR fires.
        let attempts = balances(&[("USD", 3)]);
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &attempts,
            None,
            &balances(&[("USD", 9000), ("EUR", 9000)]),
            now(),
        );
        assert_eq!(d.fired, vec![threshold("EUR", 5000)]);
        assert_eq!(d.new_attempts.get("USD"), Some(&3));
        assert_eq!(d.new_attempts.get("EUR"), Some(&1));
    }

    #[test]
    fn finished_when_all_dunned_currencies_reach_max() {
        let thresholds = vec![threshold("USD", 5000)];
        // USD at 2/3; this attempt takes it to 3 → finished.
        let attempts = balances(&[("USD", 2)]);
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &attempts,
            None,
            &balances(&[("USD", 9000)]),
            now(),
        );
        assert_eq!(d.new_attempts.get("USD"), Some(&3));
        assert!(d.finished, "single dunned currency hit max → finished");
    }

    #[test]
    fn not_finished_while_one_currency_below_max() {
        let thresholds = vec![threshold("USD", 5000), threshold("EUR", 5000)];
        let attempts = balances(&[("USD", 2), ("EUR", 0)]);
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &attempts,
            None,
            &balances(&[("USD", 9000), ("EUR", 9000)]),
            now(),
        );
        // USD → 3 (max), EUR → 1 (below max) → not finished.
        assert!(!d.finished);
    }

    #[test]
    fn per_currency_isolation() {
        let thresholds = vec![threshold("USD", 5000), threshold("EUR", 10000)];
        // USD overdue clears its threshold; EUR overdue does not.
        let d = evaluate_customer(
            policy(3, 7),
            &thresholds,
            &BTreeMap::new(),
            None,
            &balances(&[("USD", 6000), ("EUR", 9000)]),
            now(),
        );
        assert_eq!(d.fired, vec![threshold("USD", 5000)]);
        assert_eq!(d.new_attempts.get("USD"), Some(&1));
        assert_eq!(d.new_attempts.get("EUR"), None);
    }

    // ---- payment request amount + preconditions ----

    fn overdue(id: Uuid, currency: &str, total: i64, paid: i64, offset: i64) -> OverdueInvoice {
        OverdueInvoice {
            id,
            currency: currency.into(),
            total_amount_cents: total,
            total_paid_amount_cents: paid,
            credit_note_offset_cents: offset,
            payment_overdue: true,
        }
    }

    #[test]
    fn payment_request_net_amount_subtracts_paid_and_credit_offset() {
        let a = overdue(Uuid::new_v4(), "USD", 10_000, 2_000, 1_000);
        let b = overdue(Uuid::new_v4(), "USD", 5_000, 0, 500);
        // (10000-2000) + (5000-0) - (1000+500) = 8000 + 5000 - 1500 = 11_500.
        let plan = build_payment_request(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(plan.currency, "USD");
        assert_eq!(plan.amount_cents, 11_500);
        assert_eq!(plan.invoice_ids, vec![a.id, b.id]);
    }

    #[test]
    fn payment_request_net_floored_at_zero() {
        let a = overdue(Uuid::new_v4(), "USD", 1_000, 0, 5_000);
        let plan = build_payment_request(&[a]).unwrap();
        assert_eq!(
            plan.amount_cents, 0,
            "over-credited batch never goes negative"
        );
    }

    #[test]
    fn payment_request_rejects_empty() {
        assert_eq!(
            build_payment_request(&[]),
            Err(PaymentRequestError::NoInvoices)
        );
    }

    #[test]
    fn payment_request_rejects_not_overdue() {
        let mut inv = overdue(Uuid::new_v4(), "USD", 1_000, 0, 0);
        inv.payment_overdue = false;
        assert_eq!(
            build_payment_request(&[inv]),
            Err(PaymentRequestError::InvoicesNotOverdue)
        );
    }

    #[test]
    fn payment_request_rejects_mixed_currencies() {
        let a = overdue(Uuid::new_v4(), "USD", 1_000, 0, 0);
        let b = overdue(Uuid::new_v4(), "EUR", 1_000, 0, 0);
        assert_eq!(
            build_payment_request(&[a, b]),
            Err(PaymentRequestError::DifferentCurrencies)
        );
    }
}

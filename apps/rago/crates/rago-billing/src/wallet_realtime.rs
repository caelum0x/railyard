//! Real-time prepaid wallet debit through Mint holds.
//!
//! Lago only takes money from a wallet when an invoice is generated, so a
//! prepaid customer can overspend between billing cycles. A *real-time* wallet
//! closes that gap. Its money sits in the customer's Mint account, and every
//! billable event is priced as it arrives and reserved with an atomic Mint
//! hold. When the customer cannot cover it, the event is rejected or flagged,
//! following the wallet's [`RealtimePolicy`]. When the invoice is finalized,
//! the holds are captured to the seller up to the amount due and the rest is
//! released. A void releases whatever is still reserved.
//!
//! This module is the pure part: incremental event pricing with the regular
//! charge models, money conversion, deterministic idempotency keys derived from
//! the event `transaction_id`, and the capture plan. I/O lives in
//! `rago-integrations::mint_holds` (the Mint HTTP client) and
//! `rago-jobs::wallet_realtime` (orchestration).

use rago_core::models::{BillableMetric, Charge, Event, Subscription};
use rago_core::money::Currency;
use uuid::Uuid;

use crate::error::Result;

/// Mint holds are denominated in USD microdollars; one cent is 10_000.
pub const MICROS_PER_CENT: i64 = 10_000;

/// What happens to an event the wallet cannot cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimePolicy {
    /// `POST /events` answers 422 `wallet_balance_insufficient` and the event
    /// is not stored.
    Reject,
    /// The event is stored, and the shortfall is recorded and reported through
    /// `event.error`. It is billed normally on the next invoice.
    Flag,
}

impl RealtimePolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reject" => Some(Self::Reject),
            "flag" => Some(Self::Flag),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Flag => "flag",
        }
    }
}

/// Why an event could not be reserved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RealtimeError {
    /// The customer's Mint balance does not cover the event.
    #[error("wallet balance insufficient: {needed_cents} cents needed")]
    InsufficientBalance { needed_cents: i64 },
    /// Real-time wallets settle in USD (Mint is USD-denominated).
    #[error("real-time wallets must be in USD, not {0}")]
    UnsupportedCurrency(String),
    /// Mint could not be reached or refused the request for another reason.
    #[error("mint unavailable: {0}")]
    MintUnavailable(String),
}

/// Cents of a USD amount to Mint microdollars.
pub fn cents_to_micros(cents: i64, currency: &str) -> std::result::Result<i64, RealtimeError> {
    if !currency.eq_ignore_ascii_case("USD") {
        return Err(RealtimeError::UnsupportedCurrency(currency.to_uppercase()));
    }
    cents
        .checked_mul(MICROS_PER_CENT)
        .ok_or_else(|| RealtimeError::MintUnavailable("amount overflow".into()))
}

/// Microdollars back to whole cents (rounded down; captures are always whole
/// cents because holds are placed in whole cents).
pub const fn micros_to_cents(micros: i64) -> i64 {
    micros / MICROS_PER_CENT
}

/// The Mint hold idempotency key for an event: stable across retries of the
/// same `transaction_id`, distinct per wallet.
pub fn hold_idem_key(wallet_id: Uuid, transaction_id: &str) -> String {
    format!("rago:wallet:{wallet_id}:event:{transaction_id}")
}

/// The capture idempotency key for a hold settling an invoice.
pub fn capture_idem_key(hold_id: &str, invoice_id: Uuid) -> String {
    format!("rago:capture:{hold_id}:invoice:{invoice_id}")
}

/// The release idempotency key for a hold (`reason`: `void`, `surplus`,
/// `duplicate`, ...).
pub fn release_idem_key(hold_id: &str, reason: &str) -> String {
    format!("rago:release:{hold_id}:{reason}")
}

/// Price one new event: the increase of the period's charge fees on this
/// metric when `event` is added to `period_events` (the events already stored
/// for the same subscription, metric and period). Graduated, package, volume
/// and percentage models are priced exactly this way, because the marginal
/// cost depends on everything before the event. A charge whose total would
/// drop (for example a volume tier change) contributes zero, never a credit.
pub fn incremental_event_cost(
    metric: &BillableMetric,
    charges: &[Charge],
    filters: &[(uuid::Uuid, Vec<crate::charge_fees::FilterSpec>)],
    subscription: &Subscription,
    period_events: &[Event],
    event: &Event,
    currency: Currency,
) -> Result<i64> {
    let mut with_event: Vec<Event> = Vec::with_capacity(period_events.len() + 1);
    with_event.extend_from_slice(period_events);
    with_event.push(event.clone());
    let mut total = 0i64;
    for charge in charges.iter().filter(|c| c.billable_metric_id == Some(metric.id)) {
        // Filters and pricing groups price the event at its own line's rate.
        let charge_filters: &[crate::charge_fees::FilterSpec] = filters
            .iter()
            .find(|(id, _)| *id == charge.id)
            .map(|(_, f)| f.as_slice())
            .unwrap_or(&[]);
        let price = |events: &[Event]| -> Result<i64> {
            let usage = crate::charge_fees::ChargeUsage {
                metric,
                charge,
                filters: charge_filters,
                events,
                persisted_events: &[],
                period: None,
            };
            Ok(crate::charge_fees::charge_fees_all(&usage, subscription, currency, None)?
                .iter()
                .map(|f| f.amount_cents)
                .sum())
        };
        let before = price(period_events)?;
        let after = price(&with_event)?;
        total = total.saturating_add((after - before).max(0));
    }
    Ok(total)
}

/// One step of an invoice settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementStep {
    /// Capture `micros` of the hold to the seller.
    Capture { hold_id: String, micros: i64 },
    /// Release the hold entirely (the invoice is already covered).
    Release { hold_id: String },
}

/// Plan how open holds (in creation order) pay `due_micros`: holds are
/// captured in order until the amount due is covered (the last one partially;
/// Mint releases the uncaptured remainder of a hold automatically), and every
/// remaining hold is released.
pub fn plan_settlement(holds: &[(String, i64)], due_micros: i64) -> Vec<SettlementStep> {
    let mut remaining = due_micros.max(0);
    let mut steps = Vec::with_capacity(holds.len());
    for (hold_id, amount) in holds {
        if remaining > 0 && *amount > 0 {
            let take = (*amount).min(remaining);
            remaining -= take;
            steps.push(SettlementStep::Capture { hold_id: hold_id.clone(), micros: take });
        } else {
            steps.push(SettlementStep::Release { hold_id: hold_id.clone() });
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_captures_in_order_and_releases_the_surplus() {
        let holds = vec![("h1".to_string(), 300), ("h2".to_string(), 500), ("h3".to_string(), 200)];
        assert_eq!(
            plan_settlement(&holds, 600),
            vec![
                SettlementStep::Capture { hold_id: "h1".into(), micros: 300 },
                SettlementStep::Capture { hold_id: "h2".into(), micros: 300 },
                SettlementStep::Release { hold_id: "h3".into() },
            ]
        );
        assert!(plan_settlement(&holds, 0).iter().all(|s| matches!(s, SettlementStep::Release { .. })));
    }

    #[test]
    fn keys_are_stable_and_scoped() {
        let w = Uuid::from_u128(7);
        assert_eq!(hold_idem_key(w, "tx-1"), hold_idem_key(w, "tx-1"));
        assert_ne!(hold_idem_key(w, "tx-1"), hold_idem_key(Uuid::from_u128(8), "tx-1"));
        assert_ne!(capture_idem_key("h", Uuid::nil()), release_idem_key("h", "void"));
    }

    #[test]
    fn only_usd_converts_to_mint_micros() {
        assert_eq!(cents_to_micros(125, "usd"), Ok(1_250_000));
        assert_eq!(cents_to_micros(1, "EUR"), Err(RealtimeError::UnsupportedCurrency("EUR".into())));
        assert_eq!(micros_to_cents(1_259_999), 125);
        assert_eq!(RealtimePolicy::parse("flag"), Some(RealtimePolicy::Flag));
        assert_eq!(RealtimePolicy::parse("maybe"), None);
    }
}

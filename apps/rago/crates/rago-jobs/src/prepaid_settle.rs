//! Apply prepaid wallet credit to a freshly generated subscription invoice.
//!
//! Ports the `Credits::AppliedPrepaidCreditsService` call that Lago runs when a
//! subscription invoice is created for a customer holding a prepaid wallet: the
//! wallet balance is drawn down (after tax) and the invoice total reduced. The
//! recurring billing path previously skipped this, so scheduled invoices
//! over-billed customers who had prepaid credit.
//!
//! The draw-down is *computed* here (pure `apply_prepaid_credit`) but only
//! *persisted* inside the guarded invoice transaction (see
//! [`rago_db::Db::insert_subscription_invoice_with_prepaid_guarded`]), so it is
//! exactly-once with respect to the re-bill guard.

use rago_core::models::{Invoice, Wallet};
use rago_db::Db;
use uuid::Uuid;

use crate::JobError;

/// The wallet draw-down computed for one invoice: transactions to record and
/// the wallets with their decreased balances. Both are consumed by
/// [`rago_db::PrepaidSettlement`].
pub struct Settlement {
    pub transactions: Vec<rago_core::models::WalletTransaction>,
    pub updated_wallets: Vec<Wallet>,
}

impl Settlement {
    fn empty() -> Self {
        Self {
            transactions: Vec::new(),
            updated_wallets: Vec::new(),
        }
    }
}

/// Only wallets whose balance currency matches the invoice currency may fund it
/// — mixing currencies against a single cents total would mis-bill. Lago scopes
/// each wallet to the customer's currency; we enforce that explicitly here.
///
/// Returned in the caller's order (already `priority ASC, created_at ASC` from
/// [`Db::wallets_for_customer`]), which is the order prepaid credit is drawn.
pub fn wallets_matching_currency(wallets: &[Wallet], currency_code: &str) -> Vec<Wallet> {
    wallets
        .iter()
        .filter(|w| w.balance_currency.eq_ignore_ascii_case(currency_code))
        .cloned()
        .collect()
}

/// Load the customer's active wallets, apply prepaid credit to `invoice`
/// (mutating its total + `prepaid_credit_amount_cents` in place), and return the
/// draw-down to persist. A no-op when the customer has no funded wallet in the
/// invoice currency or the invoice total is already zero.
pub async fn settle_prepaid(
    db: &Db,
    customer_id: Uuid,
    invoice: &mut Invoice,
    fees: &[rago_core::models::Fee],
) -> Result<Settlement, JobError> {
    if invoice.total_amount_cents <= 0 {
        return Ok(Settlement::empty());
    }

    let currency = invoice.currency().code();
    // Real-time wallets are settled from Mint holds at finalization, never from
    // their Rago-side balance, so they are not drawn down here.
    let realtime = db.realtime_wallet_ids(customer_id).await?;
    let wallets: Vec<Wallet> = db
        .wallets_for_customer(customer_id)
        .await?
        .into_iter()
        .filter(|w| !realtime.contains(&w.id))
        .collect();
    let funding = wallets_matching_currency(&wallets, &currency);
    if funding.is_empty() {
        return Ok(Settlement::empty());
    }

    let result = rago_billing::prepaid::apply_prepaid_credit(invoice, fees, &funding);
    Ok(Settlement {
        transactions: result.transactions,
        updated_wallets: result.updated_wallets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rago_core::enums::WalletStatus;
    use rust_decimal::Decimal;

    fn wallet(currency: &str) -> Wallet {
        let now = Utc::now();
        Wallet {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            customer_id: Uuid::new_v4(),
            status: WalletStatus::Active,
            name: None,
            rate_amount: Decimal::ONE,
            credits_balance: Decimal::ZERO,
            consumed_credits: Decimal::ZERO,
            balance_cents: 5000,
            balance_currency: currency.to_string(),
            allowed_fee_types: Vec::new(),
            consumed_amount_cents: 0,
            ongoing_balance_cents: 0,
            ongoing_usage_balance_cents: 0,
            credits_ongoing_balance: Decimal::ZERO,
            credits_ongoing_usage_balance: Decimal::ZERO,
            expiration_at: None,
            terminated_at: None,
            lock_version: 0,
            priority: 50,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn keeps_only_matching_currency_wallets() {
        let wallets = vec![wallet("USD"), wallet("EUR"), wallet("usd")];
        let matched = wallets_matching_currency(&wallets, "USD");
        // Case-insensitive match keeps both USD wallets, drops the EUR one.
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|w| w.balance_currency.eq_ignore_ascii_case("USD")));
    }

    #[test]
    fn drops_all_when_no_currency_matches() {
        let wallets = vec![wallet("EUR"), wallet("GBP")];
        assert!(wallets_matching_currency(&wallets, "USD").is_empty());
    }
}

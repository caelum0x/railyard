//! Wallets & prepaid credit math.
//!
//! Faithful port of `WalletCredit` (the credit↔amount conversion that keeps
//! monetary and credit amounts consistent) and `Wallets::Balance::{Increase,
//! Decrease}Service` (top-up/grant and consumption).
//!
//! A wallet holds a balance in both *credits* and *fiat cents*, linked by
//! `rate_amount` (fiat value of one credit). Invoiceable credits round-trip
//! through the fiat amount so only whole-cent amounts are ever billed.

use chrono::Utc;
use rago_core::enums::{WalletTransactionStatus, WalletTransactionType};
use rago_core::models::{Wallet, WalletTransaction};
use rago_core::money::{round_half_up, Currency};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use uuid::Uuid;

/// `WalletCredit` — a credit expressed consistently in both credits and cents.
#[derive(Debug, Clone)]
pub struct WalletCredit {
    pub credit_amount: Decimal,
    pub amount: Decimal,
    pub amount_cents: i64,
}

impl WalletCredit {
    /// `WalletCredit.new(wallet:, credit_amount:, invoiceable:)` — construct
    /// from a number of credits.
    ///
    /// `amount = (credit_amount * rate_amount).round(exponent)`;
    /// `amount_cents = amount * subunit_to_unit`. For invoiceable credits the
    /// credit amount is re-derived from the (whole-cent) fiat amount.
    pub fn from_credit_amount(
        currency: Currency,
        rate_amount: Decimal,
        credit_amount: Decimal,
        invoiceable: bool,
    ) -> Self {
        let amount = round_half_up(credit_amount * rate_amount, currency.exponent());
        let amount_cents = (amount * currency.subunit_to_unit_decimal())
            .trunc()
            .to_i64()
            .unwrap_or(0);
        let credit_amount = if invoiceable {
            if rate_amount.is_zero() {
                Decimal::ZERO
            } else {
                amount / rate_amount
            }
        } else {
            credit_amount
        };
        Self {
            credit_amount,
            amount,
            amount_cents,
        }
    }

    /// `WalletCredit.from_amount_cents(wallet:, amount_cents:)` — construct from
    /// a fiat cent amount (used when consuming credit for an invoice).
    pub fn from_amount_cents(currency: Currency, rate_amount: Decimal, amount_cents: i64) -> Self {
        let amount = Decimal::from(amount_cents) / currency.subunit_to_unit_decimal();
        let credit_amount = if rate_amount.is_zero() {
            Decimal::ZERO
        } else {
            amount / rate_amount
        };
        Self {
            credit_amount,
            amount,
            amount_cents,
        }
    }
}

/// The transaction's fiat amount in integer cents, in the wallet's currency.
fn tx_amount_cents(wallet: &Wallet, tx: &WalletTransaction) -> i64 {
    let currency = Currency::new(&wallet.balance_currency);
    (tx.amount * currency.subunit_to_unit_decimal())
        .round()
        .to_i64()
        .unwrap_or(0)
}

/// `Wallets::Balance::IncreaseService` — apply a top-up/grant, returning the
/// updated wallet (immutable copy). Optionally resets consumed credits.
pub fn increase_balance(wallet: &Wallet, tx: &WalletTransaction, reset_consumed: bool) -> Wallet {
    let mut w = wallet.clone();
    let amount_cents = tx_amount_cents(wallet, tx);
    w.balance_cents += amount_cents;
    w.credits_balance += tx.credit_amount;
    if reset_consumed {
        w.consumed_credits = (w.consumed_credits - tx.credit_amount).max(Decimal::ZERO);
        let currency = Currency::new(&w.balance_currency);
        let reset_cents = (w.consumed_credits * w.rate_amount * currency.subunit_to_unit_decimal())
            .floor()
            .to_i64()
            .unwrap_or(0)
            .max(0);
        w.consumed_amount_cents = reset_cents;
    }
    w.updated_at = Utc::now();
    w
}

/// `Wallets::Balance::DecreaseService` — apply a consumption, returning the
/// updated wallet (immutable copy).
pub fn decrease_balance(wallet: &Wallet, tx: &WalletTransaction) -> Wallet {
    let mut w = wallet.clone();
    let amount_cents = tx_amount_cents(wallet, tx);
    w.balance_cents -= amount_cents;
    w.credits_balance -= tx.credit_amount;
    w.consumed_credits += tx.credit_amount;
    w.consumed_amount_cents += amount_cents;
    w.updated_at = Utc::now();
    w
}

/// `WalletTransactions::CreateService` — build a transaction for a credit.
#[allow(clippy::too_many_arguments)]
pub fn build_transaction(
    wallet: &Wallet,
    credit: &WalletCredit,
    transaction_type: WalletTransactionType,
    status: WalletTransactionStatus,
    invoice_id: Option<Uuid>,
) -> WalletTransaction {
    let now = Utc::now();
    WalletTransaction {
        id: Uuid::new_v4(),
        organization_id: wallet.organization_id,
        wallet_id: wallet.id,
        transaction_type,
        status,
        amount: credit.amount,
        credit_amount: credit.credit_amount,
        invoice_id,
        settled_at: matches!(status, WalletTransactionStatus::Settled).then_some(now),
        created_at: now,
        updated_at: now,
    }
}

//! Prepaid wallet credit applied to an invoice.
//!
//! Faithful port of `Credits::AllocatePrepaidCreditsByWalletsService` +
//! `Credits::AppliedPrepaidCreditsService`. Prepaid credit is applied **after
//! tax**: it draws down each wallet's balance against the invoice's fee buckets
//! (capped by the fee sub-total + taxes − credit notes), reducing the invoice
//! total. Coupons, by contrast, are applied *before* tax (see [`crate::invoices`]).

use rago_core::enums::{WalletTransactionStatus, WalletTransactionType};
use rago_core::models::{Fee, Invoice, Wallet, WalletTransaction};
use rago_core::money::Currency;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::wallets::{build_transaction, decrease_balance, WalletCredit};

/// Result of applying prepaid credit: the wallet transactions created, the
/// wallets after their balances were decreased, and the total prepaid cents.
pub struct PrepaidResult {
    pub transactions: Vec<WalletTransaction>,
    pub updated_wallets: Vec<Wallet>,
    pub prepaid_credit_amount_cents: i64,
}

/// The fiat cap a fee can absorb from prepaid credit:
/// `sub_total_excluding_taxes + taxes_precise - credit_notes` (Lago
/// `calculate_amounts_for_fees_by_type_and_bm`).
fn fee_cap_cents(fee: &Fee) -> Decimal {
    fee.sub_total_excluding_taxes_amount_cents() + fee.taxes_precise_amount_cents
        - fee.precise_credit_notes_amount_cents
}

/// `Credits::AllocatePrepaidCreditsByWalletsService` (unrestricted-wallet path):
/// per wallet, draw `min(remaining_fee_cap, remaining_wallet_balance,
/// remaining_invoice_amount)` across the fee buckets.
///
/// Wallet targeting/allowed-fee-type restrictions are catalogued in the port
/// map; this implements the common unrestricted case.
fn allocate(invoice: &Invoice, fees: &[Fee], wallets: &[Wallet]) -> Vec<(usize, i64)> {
    // Fee buckets: one cap per fee (grouping by type/BM collapses to per-fee
    // here since we do not model wallet targets yet).
    let mut caps: Vec<Decimal> = fees
        .iter()
        .map(fee_cap_cents)
        .map(|c| {
            if c.is_sign_positive() {
                c
            } else {
                Decimal::ZERO
            }
        })
        .collect();

    let mut remaining_invoice = Decimal::from(invoice.total_amount_cents);
    let mut out: Vec<(usize, i64)> = Vec::new();

    for (w_idx, wallet) in wallets.iter().enumerate() {
        if wallet.balance_cents <= 0 {
            continue;
        }
        let mut used = Decimal::ZERO;
        let wallet_balance = Decimal::from(wallet.balance_cents);

        for cap in caps.iter_mut() {
            if *cap <= Decimal::ZERO {
                continue;
            }
            let remaining_wallet = wallet_balance - used;
            if remaining_wallet <= Decimal::ZERO {
                break;
            }
            // transaction_amount = min(remaining_fee_cap, remaining_wallet, remaining_invoice)
            let txn = (*cap).min(remaining_wallet).min(remaining_invoice);
            if txn <= Decimal::ZERO {
                continue;
            }
            *cap -= txn;
            remaining_invoice -= txn;
            used += txn;
        }
        let total = used.round().to_i64().unwrap_or(0);
        if total > 0 {
            out.push((w_idx, total));
        }
    }
    out
}

/// `Credits::AppliedPrepaidCreditsService` — apply prepaid credit to the invoice
/// (after tax): create one outbound wallet transaction per drawn-down wallet,
/// decrease its balance, and reduce the invoice total.
pub fn apply_prepaid_credit(
    invoice: &mut Invoice,
    fees: &[Fee],
    wallets: &[Wallet],
) -> PrepaidResult {
    let mut transactions = Vec::new();
    let mut updated_wallets = Vec::new();
    let mut prepaid_total: i64 = 0;

    if invoice.total_amount_cents <= 0 {
        return PrepaidResult {
            transactions,
            updated_wallets: wallets.to_vec(),
            prepaid_credit_amount_cents: 0,
        };
    }

    let allocations = allocate(invoice, fees, wallets);

    for (w_idx, amount_cents) in allocations {
        let wallet = &wallets[w_idx];
        let currency = Currency::new(&wallet.balance_currency);
        let credit = WalletCredit::from_amount_cents(currency, wallet.rate_amount, amount_cents);
        let tx = build_transaction(
            wallet,
            &credit,
            WalletTransactionType::Outbound,
            WalletTransactionStatus::Settled,
            Some(invoice.id),
        );
        let updated = decrease_balance(wallet, &tx);
        prepaid_total += amount_cents;
        transactions.push(tx);
        updated_wallets.push(updated);
    }

    // AppliedPrepaidCreditsService#update_prepaid_credit_amounts +
    // CalculateFeesService#refresh_amounts: reduce the invoice total.
    invoice.prepaid_credit_amount_cents += prepaid_total;
    invoice.total_amount_cents -= prepaid_total;

    PrepaidResult {
        transactions,
        updated_wallets,
        prepaid_credit_amount_cents: prepaid_total,
    }
}

//! Purchased wallet credits — `WalletTransactions::CreateFromParamsService`
//! (paid leg) + `Invoices::PaidCreditService` + `WalletTransactions::SettleService`.
//!
//! Granted credits are free and land on the wallet at once. *Paid* credits are
//! a purchase: they create a pending inbound transaction plus a `credit`
//! invoice, the invoice is collected like any other, and the credits reach the
//! wallet only when that invoice is paid. A failed payment marks the purchase
//! failed (`wallet_transaction.payment_failure`); a later successful retry
//! still settles it.

use chrono::Utc;
use rago_billing::invoices::assemble_invoice;
use rago_billing::wallets::{build_transaction, WalletCredit};
use rago_core::enums::{
    FeeType, InvoiceStatus, InvoiceType, PaymentStatus, WalletTransactionStatus,
    WalletTransactionType,
};
use rago_core::models::{Fee, Invoice, Wallet, WalletTransaction};
use rago_core::money::Currency;
use rago_db::Db;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::scheduler::invoice_webhook_object;
use crate::webhook_emit::{record_invoice_generated, record_webhook};
use crate::JobError;
use rago_integrations::webhooks::WebhookKind;

/// The outbound webhook object of a wallet transaction.
pub fn wallet_transaction_object(tx: &WalletTransaction) -> Value {
    json!({
        "lago_id": tx.id,
        "lago_wallet_id": tx.wallet_id,
        "status": tx.status.as_str(),
        "transaction_type": tx.transaction_type.as_str(),
        "amount": tx.amount.to_string(),
        "credit_amount": tx.credit_amount.to_string(),
        "lago_invoice_id": tx.invoice_id,
        "settled_at": tx.settled_at.map(|t| t.to_rfc3339()),
        "created_at": tx.created_at.to_rfc3339(),
    })
}

/// Buy `paid_credits` for `wallet`: pending transaction + credit invoice,
/// queued for collection. Returns both.
pub async fn purchase_paid_credits(
    db: &Db,
    wallet: &Wallet,
    paid_credits: Decimal,
) -> Result<(WalletTransaction, Invoice), JobError> {
    let currency = Currency::new(&wallet.balance_currency);
    let credit = WalletCredit::from_credit_amount(currency, wallet.rate_amount, paid_credits, true);
    let invoice_id = Uuid::new_v4();
    let wtx = build_transaction(
        wallet,
        &credit,
        WalletTransactionType::Inbound,
        WalletTransactionStatus::Pending,
        Some(invoice_id),
    );

    let now = Utc::now();
    let mut fees = vec![Fee {
        id: Uuid::new_v4(),
        organization_id: wallet.organization_id,
        billing_entity_id: wallet.organization_id,
        invoice_id: Some(invoice_id),
        subscription_id: None,
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(FeeType::Credit),
        invoiceable_type: Some("WalletTransaction".into()),
        invoiceable_id: Some(wtx.id),
        amount_cents: credit.amount_cents,
        amount_currency: currency.code(),
        precise_amount_cents: Decimal::from(credit.amount_cents),
        unit_amount_cents: credit.amount_cents,
        precise_unit_amount: credit.amount,
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: Decimal::ZERO,
        taxes_rate: 0.0,
        units: credit.credit_amount,
        total_aggregated_units: None,
        events_count: None,
        precise_coupons_amount_cents: Decimal::ZERO,
        precise_credit_notes_amount_cents: Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({}),
        grouped_by: json!({}),
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }];

    let mut invoice = Invoice {
        id: invoice_id,
        organization_id: wallet.organization_id,
        billing_entity_id: wallet.organization_id,
        customer_id: Some(wallet.customer_id),
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Credit,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(now.date_naive()),
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    };
    // Prepaid credits are not taxed at purchase (they are taxed when consumed).
    assemble_invoice(&mut invoice, &mut fees, &[], &[]);

    db.insert_paid_credit_purchase(&mut invoice, &fees, &wtx).await?;

    let org = wallet.organization_id;
    record_webhook(db, org, WebhookKind::WalletTransactionCreated, wallet_transaction_object(&wtx)).await?;
    record_webhook(db, org, WebhookKind::InvoiceCreated, invoice_webhook_object(&invoice)).await?;
    record_invoice_generated(db, org, invoice.id, invoice_webhook_object(&invoice)).await?;

    if invoice.total_amount_cents > 0 {
        db.enqueue_job(
            crate::queue::JOB_COLLECT_INVOICE,
            &json!({ "invoice_id": invoice.id.to_string() }),
            Utc::now().naive_utc(),
            5,
        )
        .await?;
    } else {
        // Nothing to pay (zero-rate wallet): settle right away.
        on_invoice_payment_status(db, &invoice, PaymentStatus::Succeeded).await?;
    }
    Ok((wtx, invoice))
}

/// React to a credit invoice's payment outcome: settle the purchased credits
/// on success, mark the purchase failed on a decline. No-op for other invoices.
pub async fn on_invoice_payment_status(
    db: &Db,
    invoice: &Invoice,
    status: PaymentStatus,
) -> Result<(), JobError> {
    if !matches!(invoice.invoice_type, InvoiceType::Credit) {
        return Ok(());
    }
    let org = invoice.organization_id;
    match status {
        PaymentStatus::Succeeded => {
            let settled = db.settle_paid_credit_transactions(invoice.id).await?;
            if settled.is_empty() {
                return Ok(());
            }
            for wtx in &settled {
                record_webhook(db, org, WebhookKind::WalletTransactionUpdated, wallet_transaction_object(wtx)).await?;
            }
            record_webhook(db, org, WebhookKind::InvoicePaidCreditAdded, invoice_webhook_object(invoice)).await?;
        }
        PaymentStatus::Failed => {
            for wtx in db.fail_paid_credit_transactions(invoice.id).await? {
                record_webhook(db, org, WebhookKind::WalletTransactionPaymentFailure, wallet_transaction_object(&wtx)).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

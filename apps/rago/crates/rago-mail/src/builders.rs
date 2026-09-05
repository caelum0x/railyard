//! Email builders — render billing data into transactional emails
//! (`InvoiceMailer`, `CreditNoteMailer`, `PaymentReceiptMailer`, dunning).

use rago_core::models::{CreditNote, Fee, Invoice};
use rago_core::money::Currency;
use rust_decimal::Decimal;

use crate::Email;

fn money(cents: i64, currency: &Currency) -> String {
    let dec = Decimal::from(cents) / Decimal::from(currency.subunit_to_unit());
    format!("{} {:.*}", currency.code(), currency.exponent() as usize, dec)
}

const FROM: &str = "billing@rago.example";

/// `InvoiceMailer#finalized` — the invoice-finalized email.
pub fn invoice_finalized_email(invoice: &Invoice, fees: &[Fee], to: &str) -> Email {
    let currency = invoice.currency();
    let subject = format!("Invoice {} — {}", invoice.number, money(invoice.total_amount_cents, &currency));

    let mut rows = String::new();
    let mut text_rows = String::new();
    for fee in fees {
        let label = fee.fee_type.map(|t| t.as_str()).unwrap_or("fee");
        let amount = money(fee.amount_cents, &currency);
        rows.push_str(&format!("<tr><td>{label}</td><td style=\"text-align:right\">{amount}</td></tr>"));
        text_rows.push_str(&format!("  {label}: {amount}\n"));
    }

    let issuing = invoice.issuing_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let total = money(invoice.total_amount_cents, &currency);
    let taxes = money(invoice.taxes_amount_cents, &currency);
    let subtotal = money(invoice.sub_total_excluding_taxes_amount_cents, &currency);

    let html_body = format!(
        "<html><body>\
         <h1>Invoice {number}</h1>\
         <p>Issued {issuing} · Status: {status}</p>\
         <table>{rows}\
         <tr><td>Sub-total</td><td style=\"text-align:right\">{subtotal}</td></tr>\
         <tr><td>Taxes</td><td style=\"text-align:right\">{taxes}</td></tr>\
         <tr><td><b>Total due</b></td><td style=\"text-align:right\"><b>{total}</b></td></tr>\
         </table>\
         <p>Thank you for your business.</p>\
         </body></html>",
        number = invoice.number,
        status = invoice.status.as_str(),
    );
    let text_body = format!(
        "Invoice {number}\nIssued {issuing} · Status: {status}\n\n{text_rows}\nSub-total: {subtotal}\nTaxes: {taxes}\nTotal due: {total}\n",
        number = invoice.number,
        status = invoice.status.as_str(),
    );

    Email {
        to: to.to_string(),
        from: FROM.to_string(),
        subject,
        html_body,
        text_body,
    }
}

/// `CreditNoteMailer#created` — the credit-note-issued email.
pub fn credit_note_email(cn: &CreditNote, to: &str) -> Email {
    let currency = Currency::new(&cn.credit_amount_currency);
    let total = money(cn.total_amount_cents, &currency);
    Email {
        to: to.to_string(),
        from: FROM.to_string(),
        subject: format!("Credit note {} — {}", cn.number, total),
        html_body: format!(
            "<html><body><h1>Credit note {}</h1><p>Total: {}</p><p>Credited: {} · Refunded: {}</p></body></html>",
            cn.number, total, money(cn.credit_amount_cents, &currency), money(cn.refund_amount_cents, &currency)
        ),
        text_body: format!("Credit note {}\nTotal: {}\n", cn.number, total),
    }
}

/// `PaymentReceiptMailer` — a payment-received receipt.
pub fn payment_receipt_email(invoice: &Invoice, to: &str) -> Email {
    let currency = invoice.currency();
    let amount = money(invoice.total_amount_cents, &currency);
    Email {
        to: to.to_string(),
        from: FROM.to_string(),
        subject: format!("Payment received for invoice {}", invoice.number),
        html_body: format!(
            "<html><body><h1>Payment received</h1><p>Invoice {} · {} · Status: {}</p></body></html>",
            invoice.number, amount, invoice.payment_status.as_str()
        ),
        text_body: format!("Payment received for invoice {} ({})\n", invoice.number, amount),
    }
}

/// Dunning email for an overdue invoice.
pub fn dunning_email(invoice: &Invoice, to: &str) -> Email {
    let currency = invoice.currency();
    let amount = money(invoice.total_amount_cents, &currency);
    Email {
        to: to.to_string(),
        from: FROM.to_string(),
        subject: format!("Payment reminder — invoice {} ({} outstanding)", invoice.number, amount),
        html_body: format!(
            "<html><body><h1>Payment reminder</h1><p>Invoice {} of {} is outstanding. Please settle it at your earliest convenience.</p></body></html>",
            invoice.number, amount
        ),
        text_body: format!("Payment reminder: invoice {} of {} is outstanding.\n", invoice.number, amount),
    }
}

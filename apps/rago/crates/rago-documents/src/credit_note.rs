//! Credit-note + payment-receipt PDF rendering — reuses the dependency-free PDF
//! writer and the invoice layout, carrying the exact billing amounts.

use rago_core::models::{CreditNote, Invoice};
use rago_core::money::Currency;
use rust_decimal::Decimal;

use crate::pdf::{render, TextLine, PAGE_HEIGHT};

fn money(cents: i64, currency: &Currency) -> String {
    let sub = currency.subunit_to_unit();
    let dec = Decimal::from(cents) / Decimal::from(sub);
    let prec = currency.exponent() as usize;
    format!("{} {:.*}", currency.code(), prec, dec)
}

/// Render a credit note to a PDF document (bytes), matching Lago's credit-note
/// layout (header, invoice reference, credit/refund breakdown, totals).
pub fn credit_note_pdf(
    cn: &CreditNote,
    invoice_number: &str,
    customer_external_id: &str,
) -> Vec<u8> {
    let currency = Currency::new(&cn.total_amount_currency);
    let mut lines: Vec<TextLine> = Vec::new();
    let mut y = PAGE_HEIGHT - 60.0;
    let left = 50.0;
    let right = 400.0;

    lines.push(TextLine::new(left, y, 22.0, "CREDIT NOTE").bold());
    lines.push(TextLine::new(right, y, 12.0, format!("No. {}", cn.number)).bold());
    y -= 24.0;
    lines.push(TextLine::new(
        right,
        y,
        10.0,
        format!("Issuing date: {}", cn.issuing_date.format("%Y-%m-%d")),
    ));
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        format!("Against invoice: {invoice_number}"),
    ));
    y -= 30.0;

    lines.push(TextLine::new(left, y, 12.0, "Credited to").bold());
    y -= 16.0;
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        format!("Customer ID: {customer_external_id}"),
    ));
    y -= 30.0;

    lines.push(TextLine::new(left, y, 11.0, "Description").bold());
    lines.push(TextLine::new(right, y, 11.0, "Amount").bold());
    y -= 6.0;
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        "________________________________________________________________",
    ));
    y -= 20.0;

    let row = |lines: &mut Vec<TextLine>, y: &mut f32, label: &str, cents: i64, bold: bool| {
        let mut l = TextLine::new(300.0, *y, 10.0, label.to_string());
        let mut a = TextLine::new(right, *y, 10.0, money(cents, &currency));
        if bold {
            l = l.bold();
            a = a.bold();
        }
        lines.push(l);
        lines.push(a);
        *y -= 15.0;
    };
    row(
        &mut lines,
        &mut y,
        "Sub-total (excl. taxes)",
        cn.total_amount_cents - cn.taxes_amount_cents,
        false,
    );
    row(&mut lines, &mut y, "Taxes", cn.taxes_amount_cents, false);
    if cn.credit_amount_cents > 0 {
        row(
            &mut lines,
            &mut y,
            "Credited to wallet",
            cn.credit_amount_cents,
            false,
        );
    }
    if cn.refund_amount_cents > 0 {
        row(
            &mut lines,
            &mut y,
            "Refunded",
            cn.refund_amount_cents,
            false,
        );
    }
    y -= 4.0;
    row(
        &mut lines,
        &mut y,
        "Total credited",
        cn.total_amount_cents,
        true,
    );

    render(&lines)
}

/// Render a payment receipt for a paid invoice to a PDF document (bytes).
pub fn payment_receipt_pdf(
    invoice: &Invoice,
    customer_name: &str,
    customer_external_id: &str,
) -> Vec<u8> {
    let currency = invoice.currency();
    let mut lines: Vec<TextLine> = Vec::new();
    let mut y = PAGE_HEIGHT - 60.0;
    let left = 50.0;
    let right = 400.0;

    lines.push(TextLine::new(left, y, 22.0, "PAYMENT RECEIPT").bold());
    lines.push(TextLine::new(right, y, 12.0, format!("Invoice {}", invoice.number)).bold());
    y -= 24.0;
    if let Some(d) = invoice.issuing_date {
        lines.push(TextLine::new(
            right,
            y,
            10.0,
            format!("Issuing date: {}", d.format("%Y-%m-%d")),
        ));
    }
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        format!("Payment status: {}", invoice.payment_status.as_str()),
    ));
    y -= 30.0;

    lines.push(TextLine::new(left, y, 12.0, "Received from").bold());
    y -= 16.0;
    lines.push(TextLine::new(left, y, 10.0, customer_name.to_string()));
    y -= 13.0;
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        format!("Customer ID: {customer_external_id}"),
    ));
    y -= 30.0;

    let row = |lines: &mut Vec<TextLine>, y: &mut f32, label: &str, cents: i64, bold: bool| {
        let mut l = TextLine::new(300.0, *y, 10.0, label.to_string());
        let mut a = TextLine::new(right, *y, 10.0, money(cents, &currency));
        if bold {
            l = l.bold();
            a = a.bold();
        }
        lines.push(l);
        lines.push(a);
        *y -= 15.0;
    };
    row(
        &mut lines,
        &mut y,
        "Sub-total (excl. taxes)",
        invoice.sub_total_excluding_taxes_amount_cents,
        false,
    );
    row(
        &mut lines,
        &mut y,
        "Taxes",
        invoice.taxes_amount_cents,
        false,
    );
    y -= 4.0;
    row(
        &mut lines,
        &mut y,
        "Amount paid",
        invoice.total_amount_cents,
        true,
    );

    render(&lines)
}

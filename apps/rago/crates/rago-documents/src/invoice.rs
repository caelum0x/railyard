//! Invoice document rendering — PDF layout + XML e-invoicing output.
//!
//! Mirrors Lago's invoice template layout (header, customer, per-fee line
//! items, coupons/credits, tax and totals block) and produces the structured
//! invoice XML with the same amounts.

use rago_core::models::{Fee, Invoice};
use rago_core::money::Currency;
use rust_decimal::Decimal;

use crate::pdf::{render, TextLine, PAGE_HEIGHT};

/// Format integer cents as a currency amount string (e.g. `390.60`).
fn money(cents: i64, currency: &Currency) -> String {
    let sub = currency.subunit_to_unit();
    let dec = Decimal::from(cents) / Decimal::from(sub);
    let prec = currency.exponent() as usize;
    format!("{} {:.*}", currency.code(), prec, dec)
}

fn fee_label(fee: &Fee) -> String {
    match fee.fee_type.map(|t| t.as_str()) {
        Some("subscription") => "Subscription".to_string(),
        Some("charge") => format!("Usage charge ({} units)", fee.units.normalize()),
        Some(other) => other.to_string(),
        None => "Fee".to_string(),
    }
}

/// Render an invoice + its fees to a PDF document (bytes), matching Lago's
/// invoice layout.
pub fn invoice_pdf(
    invoice: &Invoice,
    fees: &[Fee],
    customer_name: &str,
    customer_external_id: &str,
) -> Vec<u8> {
    let lines = invoice_text_lines(invoice, fees, customer_name, customer_external_id);
    render(&lines)
}

/// Build the invoice's PDF text lines (shared by [`invoice_pdf`] and the
/// Factur-X hybrid renderer, so the visible layout is identical in both).
pub(crate) fn invoice_text_lines(
    invoice: &Invoice,
    fees: &[Fee],
    customer_name: &str,
    customer_external_id: &str,
) -> Vec<TextLine> {
    let currency = invoice.currency();
    let mut lines: Vec<TextLine> = Vec::new();
    let mut y = PAGE_HEIGHT - 60.0;
    let left = 50.0;
    let right = 400.0;

    // Header.
    lines.push(TextLine::new(left, y, 22.0, "INVOICE").bold());
    lines.push(TextLine::new(right, y, 12.0, format!("No. {}", invoice.number)).bold());
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
        format!("Status: {}", invoice.status.as_str()),
    ));
    y -= 14.0;
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        format!("Payment status: {}", invoice.payment_status.as_str()),
    ));
    y -= 30.0;

    // Customer.
    lines.push(TextLine::new(left, y, 12.0, "Bill to").bold());
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

    // Line items header.
    lines.push(TextLine::new(left, y, 11.0, "Description").bold());
    lines.push(TextLine::new(right, y, 11.0, "Amount").bold());
    y -= 6.0;
    lines.push(TextLine::new(
        left,
        y,
        10.0,
        "________________________________________________________________",
    ));
    y -= 18.0;

    // Fee line items.
    for fee in fees {
        lines.push(TextLine::new(left, y, 10.0, fee_label(fee)));
        lines.push(TextLine::new(
            right,
            y,
            10.0,
            money(fee.amount_cents, &currency),
        ));
        y -= 16.0;
    }
    y -= 8.0;

    // Totals block.
    let total_line =
        |lines: &mut Vec<TextLine>, y: &mut f32, label: &str, cents: i64, bold: bool| {
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
    total_line(
        &mut lines,
        &mut y,
        "Sub-total (excl. taxes)",
        invoice.sub_total_excluding_taxes_amount_cents,
        false,
    );
    if invoice.coupons_amount_cents > 0 {
        total_line(
            &mut lines,
            &mut y,
            "Coupons",
            -invoice.coupons_amount_cents,
            false,
        );
    }
    total_line(
        &mut lines,
        &mut y,
        "Taxes",
        invoice.taxes_amount_cents,
        false,
    );
    if invoice.credit_notes_amount_cents > 0 {
        total_line(
            &mut lines,
            &mut y,
            "Credit notes",
            -invoice.credit_notes_amount_cents,
            false,
        );
    }
    if invoice.prepaid_credit_amount_cents > 0 {
        total_line(
            &mut lines,
            &mut y,
            "Prepaid credit",
            -invoice.prepaid_credit_amount_cents,
            false,
        );
    }
    y -= 4.0;
    total_line(
        &mut lines,
        &mut y,
        "Total due",
        invoice.total_amount_cents,
        true,
    );

    lines
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render the structured invoice XML (e-invoicing), carrying the same amounts.
pub fn invoice_xml(invoice: &Invoice, fees: &[Fee], customer_external_id: &str) -> String {
    let currency = invoice.currency();
    let cur = currency.code();
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<Invoice xmlns=\"urn:lago:invoice:1\">\n");
    xml.push_str(&format!(
        "  <Number>{}</Number>\n",
        xml_escape(&invoice.number)
    ));
    if let Some(d) = invoice.issuing_date {
        xml.push_str(&format!(
            "  <IssuingDate>{}</IssuingDate>\n",
            d.format("%Y-%m-%d")
        ));
    }
    xml.push_str(&format!("  <Status>{}</Status>\n", invoice.status.as_str()));
    xml.push_str(&format!(
        "  <PaymentStatus>{}</PaymentStatus>\n",
        invoice.payment_status.as_str()
    ));
    xml.push_str(&format!("  <Currency>{cur}</Currency>\n"));
    xml.push_str(&format!(
        "  <Customer><ExternalId>{}</ExternalId></Customer>\n",
        xml_escape(customer_external_id)
    ));
    xml.push_str("  <Fees>\n");
    for fee in fees {
        xml.push_str("    <Fee>\n");
        xml.push_str(&format!(
            "      <Type>{}</Type>\n",
            fee.fee_type.map(|t| t.as_str()).unwrap_or("fee")
        ));
        xml.push_str(&format!("      <Units>{}</Units>\n", fee.units.normalize()));
        xml.push_str(&format!(
            "      <AmountCents>{}</AmountCents>\n",
            fee.amount_cents
        ));
        xml.push_str(&format!(
            "      <TaxesAmountCents>{}</TaxesAmountCents>\n",
            fee.taxes_amount_cents
        ));
        xml.push_str("    </Fee>\n");
    }
    xml.push_str("  </Fees>\n");
    xml.push_str("  <Totals>\n");
    xml.push_str(&format!(
        "    <FeesAmountCents>{}</FeesAmountCents>\n",
        invoice.fees_amount_cents
    ));
    xml.push_str(&format!(
        "    <CouponsAmountCents>{}</CouponsAmountCents>\n",
        invoice.coupons_amount_cents
    ));
    xml.push_str(&format!(
        "    <SubTotalExcludingTaxesAmountCents>{}</SubTotalExcludingTaxesAmountCents>\n",
        invoice.sub_total_excluding_taxes_amount_cents
    ));
    xml.push_str(&format!(
        "    <TaxesAmountCents>{}</TaxesAmountCents>\n",
        invoice.taxes_amount_cents
    ));
    xml.push_str(&format!(
        "    <SubTotalIncludingTaxesAmountCents>{}</SubTotalIncludingTaxesAmountCents>\n",
        invoice.sub_total_including_taxes_amount_cents
    ));
    xml.push_str(&format!(
        "    <CreditNotesAmountCents>{}</CreditNotesAmountCents>\n",
        invoice.credit_notes_amount_cents
    ));
    xml.push_str(&format!(
        "    <PrepaidCreditAmountCents>{}</PrepaidCreditAmountCents>\n",
        invoice.prepaid_credit_amount_cents
    ));
    xml.push_str(&format!(
        "    <TotalAmountCents>{}</TotalAmountCents>\n",
        invoice.total_amount_cents
    ));
    xml.push_str("  </Totals>\n");
    xml.push_str("</Invoice>\n");
    xml
}

//! Document generation tests: valid PDF + XML amounts for a 39060c invoice.

use chrono::{NaiveDate, Utc};
use rago_core::enums::{FeeType, InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Fee, Invoice};
use rago_documents::{invoice_pdf, invoice_xml};
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn fee(fee_type: FeeType, amount_cents: i64, units: rust_decimal::Decimal, taxes: i64) -> Fee {
    Fee {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        invoice_id: None,
        subscription_id: None,
        charge_id: None,
        charge_filter_id: None,
        fee_type: Some(fee_type),
        invoiceable_type: None,
        invoiceable_id: None,
        amount_cents,
        amount_currency: "USD".into(),
        precise_amount_cents: rust_decimal::Decimal::from(amount_cents),
        unit_amount_cents: 0,
        precise_unit_amount: dec!(0),
        taxes_amount_cents: taxes,
        taxes_precise_amount_cents: rust_decimal::Decimal::from(taxes),
        taxes_rate: 8.5,
        units,
        total_aggregated_units: Some(units),
        events_count: None,
        precise_coupons_amount_cents: dec!(0),
        precise_credit_notes_amount_cents: dec!(0),
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: json!({}),
        grouped_by: json!({}),
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

fn invoice_39060() -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        customer_id: Some(Uuid::new_v4()),
        number: "INV-DEMO-1".into(),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: Some(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap()),
        fees_amount_cents: 36000,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 36000,
        sub_total_including_taxes_amount_cents: 39060,
        taxes_amount_cents: 3060,
        taxes_rate: 8.5,
        total_amount_cents: 39060,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

#[test]
fn pdf_is_valid_and_has_totals() {
    let inv = invoice_39060();
    let fees = vec![
        fee(FeeType::Subscription, 31000, dec!(1), 2635),
        fee(FeeType::Charge, 5000, dec!(100), 425),
    ];
    let pdf = invoice_pdf(&inv, &fees, "Acme Co", "cust_1");

    // Valid PDF: magic header + EOF marker + xref/trailer.
    assert!(pdf.starts_with(b"%PDF-1.4"), "PDF magic bytes");
    let tail = String::from_utf8_lossy(&pdf[pdf.len().saturating_sub(64)..]);
    assert!(tail.contains("%%EOF"), "PDF EOF marker");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("startxref"));
    assert!(text.contains("/Type /Catalog"));

    // The rendered content carries the invoice number + total.
    assert!(text.contains("INV-DEMO-1"), "invoice number in content");
    assert!(text.contains("Total due"), "totals label");
    assert!(text.contains("USD 390.60"), "total 39060c formatted");
    assert!(text.contains("USD 310.00"), "subscription fee");
    assert!(pdf.len() > 600);
}

#[test]
fn xml_has_correct_amounts() {
    let inv = invoice_39060();
    let fees = vec![
        fee(FeeType::Subscription, 31000, dec!(1), 2635),
        fee(FeeType::Charge, 5000, dec!(100), 425),
    ];
    let xml = invoice_xml(&inv, &fees, "cust_1");
    assert!(xml.starts_with("<?xml version=\"1.0\""));
    assert!(xml.contains("<Number>INV-DEMO-1</Number>"));
    assert!(xml.contains("<TotalAmountCents>39060</TotalAmountCents>"));
    assert!(xml.contains("<TaxesAmountCents>3060</TaxesAmountCents>"));
    assert!(xml
        .contains("<SubTotalExcludingTaxesAmountCents>36000</SubTotalExcludingTaxesAmountCents>"));
    assert!(xml.contains("<AmountCents>31000</AmountCents>"));
    assert!(xml.contains("<AmountCents>5000</AmountCents>"));
    assert!(xml.contains("<ExternalId>cust_1</ExternalId>"));
}

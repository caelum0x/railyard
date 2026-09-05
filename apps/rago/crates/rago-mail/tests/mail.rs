//! Email builder + injectable transport tests.

use chrono::{NaiveDate, Utc};
use rago_core::enums::{FeeType, InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Fee, Invoice};
use rago_mail::builders::invoice_finalized_email;
use rago_mail::{CaptureTransport, EmailTransport};
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn fee(ft: FeeType, amount: i64) -> Fee {
    Fee {
        id: Uuid::new_v4(), organization_id: Uuid::nil(), billing_entity_id: Uuid::nil(),
        invoice_id: None, subscription_id: None, charge_id: None, charge_filter_id: None,
        fee_type: Some(ft), invoiceable_type: None, invoiceable_id: None,
        amount_cents: amount, amount_currency: "USD".into(),
        precise_amount_cents: rust_decimal::Decimal::from(amount), unit_amount_cents: 0,
        precise_unit_amount: dec!(0), taxes_amount_cents: 0, taxes_precise_amount_cents: dec!(0),
        taxes_rate: 0.0, units: dec!(1), total_aggregated_units: Some(dec!(1)), events_count: None,
        precise_coupons_amount_cents: dec!(0), precise_credit_notes_amount_cents: dec!(0),
        payment_status: PaymentStatus::Pending, properties: json!({}), amount_details: json!({}),
        grouped_by: json!({}), created_at: Utc::now(), updated_at: Utc::now(), applied_taxes: Vec::new(),
    }
}

fn invoice() -> Invoice {
    Invoice {
        id: Uuid::new_v4(), organization_id: Uuid::nil(), billing_entity_id: Uuid::nil(),
        customer_id: None, number: "INV-9".into(), sequential_id: Some(9),
        invoice_type: InvoiceType::Subscription, status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending, currency: Some("USD".into()),
        issuing_date: Some(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap()),
        fees_amount_cents: 36000, coupons_amount_cents: 0, credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0, progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 36000, sub_total_including_taxes_amount_cents: 39060,
        taxes_amount_cents: 3060, taxes_rate: 8.5, total_amount_cents: 39060, version_number: 4,
        created_at: Utc::now(), updated_at: Utc::now(), applied_taxes: Vec::new(),
    }
}

#[tokio::test]
async fn invoice_email_rendered_and_sent() {
    let inv = invoice();
    let fees = vec![fee(FeeType::Subscription, 31000), fee(FeeType::Charge, 5000)];
    let email = invoice_finalized_email(&inv, &fees, "alice@acme.test");

    assert_eq!(email.to, "alice@acme.test");
    assert!(email.subject.contains("INV-9"));
    assert!(email.subject.contains("USD 390.60"));
    assert!(email.html_body.contains("Total due"));
    assert!(email.html_body.contains("USD 390.60"));
    assert!(email.text_body.contains("Taxes: USD 30.60"));
    assert!(email.text_body.contains("Total due: USD 390.60"));

    let transport = CaptureTransport::new();
    transport.send(&email).await.unwrap();
    let sent = transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].subject, email.subject);
}

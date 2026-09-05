//! Real e-invoicing standards tests: UBL 2.1 (invoice/credit-note/receipt),
//! PEPPOL BIS Billing 3.0, UN/CEFACT CII (Factur-X), and XRechnung 3.0.
//!
//! Each format asserts the correct root element + namespaces + the profile
//! `CustomizationID`/`ProfileID`, and the key monetary totals matching the
//! invoice's numbers (line extension = sum of lines, tax total correct,
//! payable = tax-inclusive − prepaid). Every document is re-parsed for
//! well-formedness — the dependency-free stand-in for full XSD validation.

use chrono::{NaiveDate, Utc};
use rago_core::enums::{FeeType, InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{
    CreditNote, CreditNoteAppliedTax, CreditNoteItem, Customer, Fee, Invoice, Organization,
};
use rago_documents::xml::is_well_formed;
use rago_documents::{
    cii_credit_note_xml, cii_invoice_xml, facturx_cii_xml, facturx_pdf, peppol_credit_note_xml,
    peppol_invoice_xml, ubl_credit_note_xml, ubl_invoice_xml, ubl_receipt_xml,
    xrechnung_cii_invoice_xml, xrechnung_ubl_invoice_xml, Profile,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn supplier() -> Organization {
    Organization {
        id: Uuid::nil(),
        name: "Rago Billing Inc".into(),
        slug: "rago-billing".into(),
        default_currency: "USD".into(),
        email: Some("billing@rago.example".into()),
        country: Some("US".into()),
        tax_identification_number: Some("US123456789".into()),
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: false,
        hmac_key: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn customer() -> Customer {
    Customer {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        external_id: "cust_1".into(),
        name: Some("Acme Co".into()),
        email: Some("ap@acme.example".into()),
        country: Some("DE".into()),
        currency: Some("USD".into()),
        timezone: None,
        tax_identification_number: Some("DE987654321".into()),
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    }
}

fn fee(fee_type: FeeType, amount_cents: i64, units: Decimal, taxes: i64) -> Fee {
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
        precise_amount_cents: Decimal::from(amount_cents),
        unit_amount_cents: 0,
        precise_unit_amount: Decimal::from(amount_cents) / dec!(100),
        taxes_amount_cents: taxes,
        taxes_precise_amount_cents: Decimal::from(taxes),
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

fn demo_fees() -> Vec<Fee> {
    vec![
        fee(FeeType::Subscription, 31000, dec!(1), 2635),
        fee(FeeType::Charge, 5000, dec!(100), 425),
    ]
}

fn credit_note_10850() -> CreditNote {
    let fee_id = Uuid::new_v4();
    CreditNote {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: Uuid::new_v4(),
        invoice_id: Uuid::new_v4(),
        sequential_id: 1,
        number: "CN-DEMO-1".into(),
        issuing_date: NaiveDate::from_ymd_opt(2024, 3, 1).unwrap(),
        reason: 0,
        credit_status: None,
        refund_status: None,
        status: 1,
        credit_amount_cents: 10850,
        credit_amount_currency: "USD".into(),
        balance_amount_cents: 10850,
        refund_amount_cents: 0,
        total_amount_cents: 10850,
        total_amount_currency: "USD".into(),
        taxes_amount_cents: 850,
        taxes_rate: 8.5,
        coupons_adjustment_amount_cents: 0,
        precise_coupons_adjustment_amount_cents: dec!(0),
        precise_taxes_amount_cents: dec!(850),
        created_at: now(),
        updated_at: now(),
        voided_at: None,
        items: vec![CreditNoteItem {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            credit_note_id: None,
            fee_id,
            amount_cents: 10000,
            precise_amount_cents: dec!(10000),
            amount_currency: "USD".into(),
        }],
        applied_taxes: vec![CreditNoteAppliedTax {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            credit_note_id: None,
            tax_id: Uuid::new_v4(),
            tax_code: "vat".into(),
            tax_name: "VAT".into(),
            tax_rate: 8.5,
            tax_description: None,
            amount_cents: 850,
            base_amount_cents: 10000,
            amount_currency: "USD".into(),
        }],
    }
}

// ---- UBL invoice ----------------------------------------------------------

#[test]
fn ubl_invoice_en16931_root_ns_and_totals() {
    let inv = invoice_39060();
    let xml = ubl_invoice_xml(
        &inv,
        &demo_fees(),
        &supplier(),
        &customer(),
        Profile::En16931,
    );
    is_well_formed(&xml).expect("UBL invoice must be well-formed");

    // Root element + namespaces.
    assert!(xml.contains("<Invoice "));
    assert!(xml.contains("xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:Invoice-2\""));
    assert!(xml.contains(
        "xmlns:cac=\"urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2\""
    ));
    assert!(xml.contains(
        "xmlns:cbc=\"urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2\""
    ));

    // Profile.
    assert!(xml.contains("<cbc:UBLVersionID>2.1</cbc:UBLVersionID>"));
    assert!(xml.contains("<cbc:CustomizationID>urn:cen.eu:en16931:2017</cbc:CustomizationID>"));
    assert!(!xml.contains("<cbc:ProfileID>")); // EN16931 has no ProfileID
    assert!(xml.contains("<cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>"));
    assert!(xml.contains("<cbc:ID>INV-DEMO-1</cbc:ID>"));

    // Monetary parity.
    assert!(xml
        .contains("<cbc:LineExtensionAmount currencyID=\"USD\">360.00</cbc:LineExtensionAmount>"));
    assert!(
        xml.contains("<cbc:TaxExclusiveAmount currencyID=\"USD\">360.00</cbc:TaxExclusiveAmount>")
    );
    assert!(
        xml.contains("<cbc:TaxInclusiveAmount currencyID=\"USD\">390.60</cbc:TaxInclusiveAmount>")
    );
    assert!(xml.contains("<cbc:PayableAmount currencyID=\"USD\">390.60</cbc:PayableAmount>"));
    // TaxTotal.
    assert!(xml.contains("<cbc:TaxAmount currencyID=\"USD\">30.60</cbc:TaxAmount>"));
    assert!(xml.contains("<cbc:Percent>8.50</cbc:Percent>"));

    // payable == tax_inclusive - prepaid (prepaid = 0 here).
    let prepaid = inv.prepaid_credit_amount_cents + inv.credit_notes_amount_cents;
    assert_eq!(
        inv.sub_total_including_taxes_amount_cents - prepaid,
        inv.total_amount_cents
    );
}

#[test]
fn ubl_invoice_lines_sum_to_line_extension() {
    let inv = invoice_39060();
    let fees = demo_fees();
    let xml = ubl_invoice_xml(&inv, &fees, &supplier(), &customer(), Profile::En16931);
    // Two invoice lines with the fee amounts.
    assert_eq!(xml.matches("<cac:InvoiceLine>").count(), 2);
    assert!(xml
        .contains("<cbc:LineExtensionAmount currencyID=\"USD\">310.00</cbc:LineExtensionAmount>"));
    assert!(
        xml.contains("<cbc:LineExtensionAmount currencyID=\"USD\">50.00</cbc:LineExtensionAmount>")
    );
    // Sum of line extensions == invoice fees_amount.
    let sum: i64 = fees.iter().map(|f| f.amount_cents).sum();
    assert_eq!(sum, inv.fees_amount_cents);
}

// ---- PEPPOL BIS 3.0 -------------------------------------------------------

#[test]
fn peppol_invoice_has_bis_customization_and_profile() {
    let xml = peppol_invoice_xml(&invoice_39060(), &demo_fees(), &supplier(), &customer());
    is_well_formed(&xml).unwrap();
    assert!(xml.contains(
        "<cbc:CustomizationID>urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0</cbc:CustomizationID>"
    ));
    assert!(
        xml.contains("<cbc:ProfileID>urn:fdc:peppol.eu:2017:poacc:billing:01:1.0</cbc:ProfileID>")
    );
    // BIS mandates BuyerReference.
    assert!(xml.contains("<cbc:BuyerReference>cust_1</cbc:BuyerReference>"));
    // Endpoint identifiers for supplier + customer.
    assert!(xml.contains("<cbc:EndpointID schemeID=\"EM\">billing@rago.example</cbc:EndpointID>"));
    assert!(xml.contains("<cbc:EndpointID schemeID=\"EM\">ap@acme.example</cbc:EndpointID>"));
}

// ---- XRechnung 3.0 --------------------------------------------------------

#[test]
fn xrechnung_ubl_customization_and_buyer_reference() {
    let xml = xrechnung_ubl_invoice_xml(&invoice_39060(), &demo_fees(), &supplier(), &customer());
    is_well_formed(&xml).unwrap();
    assert!(xml.contains(
        "<cbc:CustomizationID>urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0</cbc:CustomizationID>"
    ));
    assert!(xml.contains("<cbc:BuyerReference>cust_1</cbc:BuyerReference>"));
    assert!(xml.contains("<Invoice "));
}

#[test]
fn xrechnung_cii_customization_in_guideline_parameter() {
    let xml = xrechnung_cii_invoice_xml(&invoice_39060(), &demo_fees(), &supplier(), &customer());
    is_well_formed(&xml).unwrap();
    assert!(xml.contains("<rsm:CrossIndustryInvoice"));
    assert!(xml.contains(
        "<ram:ID>urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0</ram:ID>"
    ));
}

// ---- CII / Factur-X -------------------------------------------------------

#[test]
fn cii_invoice_root_ns_and_totals() {
    let xml = cii_invoice_xml(
        &invoice_39060(),
        &demo_fees(),
        &supplier(),
        &customer(),
        Profile::En16931,
    );
    is_well_formed(&xml).unwrap();

    assert!(xml.contains("<rsm:CrossIndustryInvoice"));
    assert!(
        xml.contains("xmlns:rsm=\"urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100\"")
    );
    assert!(xml.contains("xmlns:ram=\"urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100\""));
    assert!(
        xml.contains("xmlns:udt=\"urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100\"")
    );
    assert!(xml.contains("<ram:ID>urn:cen.eu:en16931:2017</ram:ID>"));
    assert!(xml.contains("<ram:TypeCode>380</ram:TypeCode>"));

    // Monetary summation parity.
    assert!(xml.contains("<ram:LineTotalAmount>360.00</ram:LineTotalAmount>"));
    assert!(xml.contains("<ram:TaxBasisTotalAmount>360.00</ram:TaxBasisTotalAmount>"));
    assert!(xml.contains("<ram:TaxTotalAmount currencyID=\"USD\">30.60</ram:TaxTotalAmount>"));
    assert!(xml.contains("<ram:GrandTotalAmount>390.60</ram:GrandTotalAmount>"));
    assert!(xml.contains("<ram:DuePayableAmount>390.60</ram:DuePayableAmount>"));
    // CII date format.
    assert!(xml.contains("<udt:DateTimeString format=\"102\">20240201</udt:DateTimeString>"));
}

#[test]
fn facturx_pdf_embeds_cii_xml() {
    let inv = invoice_39060();
    let fees = demo_fees();
    let sup = supplier();
    let cust = customer();
    let cii = facturx_cii_xml(&inv, &fees, &sup, &cust);
    is_well_formed(&cii).unwrap();

    let pdf = facturx_pdf(&inv, &fees, &sup, &cust);
    assert!(pdf.starts_with(b"%PDF-1.4"), "valid PDF header");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("%%EOF"));
    // Associated Factur-X file, embedded as invoice data.
    assert!(text.contains("factur-x.xml"), "embedded filename");
    assert!(text.contains("/AFRelationship /Data"), "associated as data");
    assert!(text.contains("/Type /EmbeddedFile"), "embedded file stream");
    assert!(
        text.contains("/AF [8 0 R]"),
        "document-level associated files"
    );
    // The CII payload bytes are present inside the PDF.
    assert!(text.contains("<rsm:CrossIndustryInvoice"), "CII embedded");
    // The visible invoice layout is preserved.
    assert!(text.contains("INV-DEMO-1"));
    assert!(text.contains("USD 390.60"));
}

// ---- Credit notes ---------------------------------------------------------

#[test]
fn ubl_credit_note_root_reference_and_negative_totals() {
    let cn = credit_note_10850();
    let issue = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
    let xml = ubl_credit_note_xml(
        &cn,
        &supplier(),
        &customer(),
        "INV-DEMO-1",
        issue,
        Profile::En16931,
    );
    is_well_formed(&xml).unwrap();

    assert!(xml.contains("<CreditNote "));
    assert!(xml.contains("xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2\""));
    assert!(xml.contains("<cbc:CreditNoteTypeCode>381</cbc:CreditNoteTypeCode>"));
    // Reference to the original invoice.
    assert!(xml.contains("<cac:BillingReference>"));
    assert!(xml.contains("<cbc:ID>INV-DEMO-1</cbc:ID>"));
    // Negative monetary totals.
    assert!(xml
        .contains("<cbc:LineExtensionAmount currencyID=\"USD\">-100.00</cbc:LineExtensionAmount>"));
    assert!(
        xml.contains("<cbc:TaxInclusiveAmount currencyID=\"USD\">-108.50</cbc:TaxInclusiveAmount>")
    );
    assert!(xml.contains("<cbc:PayableAmount currencyID=\"USD\">-108.50</cbc:PayableAmount>"));
    assert!(xml.contains("<cbc:TaxAmount currencyID=\"USD\">-8.50</cbc:TaxAmount>"));
    assert_eq!(cn.items.len(), 1);
}

#[test]
fn peppol_credit_note_has_bis_profile() {
    let cn = credit_note_10850();
    let issue = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
    let xml = peppol_credit_note_xml(&cn, &supplier(), &customer(), "INV-DEMO-1", issue);
    is_well_formed(&xml).unwrap();
    assert!(xml.contains("peppol.eu:2017:poacc:billing:3.0"));
    assert!(
        xml.contains("<cbc:ProfileID>urn:fdc:peppol.eu:2017:poacc:billing:01:1.0</cbc:ProfileID>")
    );
}

#[test]
fn cii_credit_note_negative_totals() {
    let cn = credit_note_10850();
    let xml = cii_credit_note_xml(
        &cn,
        &supplier(),
        &customer(),
        "INV-DEMO-1",
        Profile::En16931,
    );
    is_well_formed(&xml).unwrap();
    assert!(xml.contains("<rsm:CrossIndustryInvoice"));
    assert!(xml.contains("<ram:TypeCode>381</ram:TypeCode>"));
    assert!(xml.contains("<ram:LineTotalAmount>-100.00</ram:LineTotalAmount>"));
    assert!(xml.contains("<ram:GrandTotalAmount>-108.50</ram:GrandTotalAmount>"));
    assert!(xml.contains("<ram:DuePayableAmount>-108.50</ram:DuePayableAmount>"));
}

// ---- Receipt --------------------------------------------------------------

#[test]
fn ubl_receipt_application_response_paid() {
    let inv = invoice_39060();
    let paid = NaiveDate::from_ymd_opt(2024, 2, 15).unwrap();
    let xml = ubl_receipt_xml(&inv, &supplier(), &customer(), "RCPT-1", paid);
    is_well_formed(&xml).unwrap();

    assert!(xml.contains("<ApplicationResponse "));
    assert!(xml
        .contains("xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:ApplicationResponse-2\""));
    assert!(xml.contains("<cbc:ID>RCPT-1</cbc:ID>"));
    assert!(xml.contains("<cbc:ResponseCode>PD</cbc:ResponseCode>"));
    // References the paid invoice + its amount in the note.
    assert!(xml.contains("<cbc:ID>INV-DEMO-1</cbc:ID>"));
    assert!(xml.contains("USD 390.60"));
}

// ---- Cross-format well-formedness sweep -----------------------------------

#[test]
fn all_formats_are_well_formed() {
    let inv = invoice_39060();
    let fees = demo_fees();
    let sup = supplier();
    let cust = customer();
    let cn = credit_note_10850();
    let issue = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
    let paid = NaiveDate::from_ymd_opt(2024, 2, 15).unwrap();

    let docs = vec![
        ubl_invoice_xml(&inv, &fees, &sup, &cust, Profile::En16931),
        peppol_invoice_xml(&inv, &fees, &sup, &cust),
        xrechnung_ubl_invoice_xml(&inv, &fees, &sup, &cust),
        xrechnung_cii_invoice_xml(&inv, &fees, &sup, &cust),
        cii_invoice_xml(&inv, &fees, &sup, &cust, Profile::En16931),
        facturx_cii_xml(&inv, &fees, &sup, &cust),
        ubl_credit_note_xml(&cn, &sup, &cust, "INV-DEMO-1", issue, Profile::En16931),
        peppol_credit_note_xml(&cn, &sup, &cust, "INV-DEMO-1", issue),
        cii_credit_note_xml(&cn, &sup, &cust, "INV-DEMO-1", Profile::En16931),
        ubl_receipt_xml(&inv, &sup, &cust, "RCPT-1", paid),
    ];
    for (i, d) in docs.iter().enumerate() {
        is_well_formed(d).unwrap_or_else(|e| panic!("doc {i} not well-formed: {e}"));
    }
}

//! PEPPOL BIS Billing 3.0 — the profile most European access points require.
//!
//! BIS Billing 3.0 is EN 16931 in UBL syntax with the PEPPOL `CustomizationID`
//! (`urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0`)
//! and `ProfileID` (`urn:fdc:peppol.eu:2017:poacc:billing:01:1.0`), plus the
//! mandatory `BuyerReference` and endpoint identifiers. These builders are the
//! UBL serializers pinned to [`Profile::PeppolBis3`].

use rago_core::models::{CreditNote, Customer, Fee, Invoice, Organization};

use crate::einvoice::Profile;
use crate::ubl::{ubl_credit_note_xml, ubl_invoice_xml};

/// A PEPPOL BIS Billing 3.0 conformant `Invoice`.
pub fn peppol_invoice_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
) -> String {
    ubl_invoice_xml(invoice, fees, supplier, customer, Profile::PeppolBis3)
}

/// A PEPPOL BIS Billing 3.0 conformant `CreditNote`.
pub fn peppol_credit_note_xml(
    cn: &CreditNote,
    supplier: &Organization,
    customer: &Customer,
    invoice_number: &str,
    invoice_issue_date: chrono::NaiveDate,
) -> String {
    ubl_credit_note_xml(
        cn,
        supplier,
        customer,
        invoice_number,
        invoice_issue_date,
        Profile::PeppolBis3,
    )
}

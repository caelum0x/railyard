//! XRechnung 3.0 — the German CIUS of EN 16931, mandatory for invoicing German
//! public authorities. It is EN 16931 in either UBL or CII syntax carrying the
//! XRechnung `CustomizationID`
//! (`urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0`)
//! and the customer's Leitweg-ID in `BuyerReference`. These builders pin the
//! UBL / CII serializers to [`Profile::XRechnung30`].

use rago_core::models::{Customer, Fee, Invoice, Organization};

use crate::cii::cii_invoice_xml;
use crate::einvoice::Profile;
use crate::ubl::ubl_invoice_xml;

/// XRechnung 3.0 invoice in UBL syntax.
pub fn xrechnung_ubl_invoice_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
) -> String {
    ubl_invoice_xml(invoice, fees, supplier, customer, Profile::XRechnung30)
}

/// XRechnung 3.0 invoice in CII syntax.
pub fn xrechnung_cii_invoice_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
) -> String {
    cii_invoice_xml(invoice, fees, supplier, customer, Profile::XRechnung30)
}

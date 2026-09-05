//! Factur-X / ZUGFeRD — the Franco-German hybrid e-invoice: a human-readable
//! PDF with the machine-readable EN 16931 **CII** XML embedded as an associated
//! file (`factur-x.xml`, `/AFRelationship /Data`).
//!
//! [`facturx_cii_xml`] produces the CII payload (EN 16931 / "EN 16931" comfort
//! profile). [`facturx_pdf`] renders the existing invoice PDF and embeds that
//! payload via [`crate::pdf::render_with_attachment`], yielding the hybrid
//! container. Strict PDF/A-3 additionally needs XMP metadata + an OutputIntent,
//! which the dependency-free PDF writer does not emit (follow-up).

use rago_core::models::{Customer, Fee, Invoice, Organization};

use crate::cii::cii_invoice_xml;
use crate::einvoice::Profile;
use crate::invoice::invoice_text_lines;
use crate::pdf::render_with_attachment;

/// The conventional filename for the embedded Factur-X CII payload.
pub const FACTURX_FILENAME: &str = "factur-x.xml";

/// The EN 16931 CII XML for a Rago invoice (the Factur-X companion payload).
pub fn facturx_cii_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
) -> String {
    cii_invoice_xml(invoice, fees, supplier, customer, Profile::En16931)
}

/// A hybrid Factur-X PDF: the invoice PDF layout with the CII XML embedded as
/// an associated file. Re-renders the same text lines as [`invoice_pdf`] so the
/// visible document and the embedded data stay identical.
pub fn facturx_pdf(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
) -> Vec<u8> {
    let xml = facturx_cii_xml(invoice, fees, supplier, customer);
    let customer_name = customer.name.as_deref().unwrap_or(&customer.external_id);
    let lines = invoice_text_lines(invoice, fees, customer_name, &customer.external_id);
    render_with_attachment(&lines, FACTURX_FILENAME, xml.as_bytes())
}

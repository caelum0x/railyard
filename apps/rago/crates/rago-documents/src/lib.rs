//! # rago-documents
//!
//! Invoice/credit-note document generation for Rago: a dependency-free PDF
//! writer ([`pdf`]) and the invoice renderer ([`invoice`]) that produces both a
//! real PDF (matching Lago's invoice layout) and the structured invoice XML —
//! both carrying the exact billing amounts.
//!
//! ## Real e-invoicing standards
//!
//! Beyond the legacy `urn:lago:invoice:1` XML ([`invoice::invoice_xml`]), the
//! crate serializes the actual EN 16931 e-invoicing formats, all mapped from
//! Rago's invoice/fee/tax model (field mapping mirrors Lago's
//! `app/serializers/e_invoices/*`):
//!
//! * [`ubl`] — OASIS **UBL 2.1** `Invoice`, `CreditNote`, and the
//!   `ApplicationResponse` payment receipt.
//! * [`peppol`] — **PEPPOL BIS Billing 3.0** (UBL + the PEPPOL
//!   `CustomizationID`/`ProfileID`).
//! * [`cii`] — UN/CEFACT **Cross-Industry Invoice** (the Factur-X/ZUGFeRD and
//!   CII-XRechnung syntax).
//! * [`facturx`] — **Factur-X / ZUGFeRD** hybrid: the CII XML embedded in the
//!   invoice PDF as an associated file.
//! * [`xrechnung`] — **XRechnung 3.0** (German CIUS) in UBL or CII syntax.
//!
//! Well-formedness of every document is checkable with [`xml::is_well_formed`].
//! Full XSD/Schematron validation and a live PEPPOL access point are follow-ups.

pub mod cii;
pub mod credit_note;
pub mod einvoice;
pub mod facturx;
pub mod invoice;
pub mod pdf;
pub mod peppol;
pub mod ubl;
pub mod xml;
pub mod xrechnung;

pub use credit_note::{credit_note_pdf, payment_receipt_pdf};
pub use einvoice::Profile;
pub use invoice::{invoice_pdf, invoice_xml};

// Real-standard e-invoicing serializers.
pub use cii::{cii_credit_note_xml, cii_invoice_xml};
pub use facturx::{facturx_cii_xml, facturx_pdf};
pub use peppol::{peppol_credit_note_xml, peppol_invoice_xml};
pub use ubl::{ubl_credit_note_xml, ubl_invoice_xml, ubl_receipt_xml};
pub use xrechnung::{xrechnung_cii_invoice_xml, xrechnung_ubl_invoice_xml};

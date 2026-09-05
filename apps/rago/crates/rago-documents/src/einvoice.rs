//! Shared e-invoicing primitives: standard profile IDs, UNTDID/UNCL code
//! constants, currency-aware amount formatting, and the per-tax-rate grouping
//! that UBL `TaxSubtotal` / CII `ApplicableTradeTax` both need.
//!
//! Field mapping mirrors Lago's `app/serializers/e_invoices/*` (the invoice
//! data model is the source of truth); the profile IDs are the real published
//! URNs for EN 16931, PEPPOL BIS Billing 3.0, and XRechnung 3.0.

use rago_core::enums::InvoiceType;
use rago_core::models::Fee;
use rago_core::money::{round_half_up, Currency};
use rust_decimal::Decimal;

// ---- Namespaces -----------------------------------------------------------

pub const UBL_INVOICE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:Invoice-2";
pub const UBL_CREDIT_NOTE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2";
pub const UBL_RESPONSE_NS: &str =
    "urn:oasis:names:specification:ubl:schema:xsd:ApplicationResponse-2";
pub const UBL_CAC_NS: &str =
    "urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2";
pub const UBL_CBC_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2";

pub const CII_RSM_NS: &str = "urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100";
pub const CII_QDT_NS: &str = "urn:un:unece:uncefact:data:standard:QualifiedDataType:100";
pub const CII_RAM_NS: &str =
    "urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100";
pub const CII_UDT_NS: &str = "urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100";
pub const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";

// ---- Code lists -----------------------------------------------------------

/// UNTDID 1001 document type codes.
pub const COMMERCIAL_INVOICE: i32 = 380;
pub const CREDIT_NOTE: i32 = 381;
pub const PREPAID_INVOICE: i32 = 386;
pub const PAYMENT_RECEIPT: i32 = 202;

/// UNTDID 4461 payment means codes.
pub const STANDARD_PAYMENT: i32 = 1;

/// UNTDID 5153 tax type.
pub const VAT: &str = "VAT";
/// UNCL 5305 tax category codes.
pub const S_CATEGORY: &str = "S";
pub const O_CATEGORY: &str = "O";
pub const Z_CATEGORY: &str = "Z";
/// PEPPOL VATEX exemption code for out-of-scope supplies.
pub const O_VAT_EXEMPTION: &str = "VATEX-EU-O";
/// UNECE Rec. 20 unit code — "one" (piece).
pub const UNIT_CODE: &str = "C62";
/// ApplicationResponse response code — paid.
pub const PAID_RESPONSE: &str = "PD";

// ---- Profiles -------------------------------------------------------------

/// EN 16931 profile / CIUS selection. Drives the UBL `cbc:CustomizationID`
/// (and `cbc:ProfileID`) and the CII `GuidelineSpecifiedDocumentContextParameter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Plain EN 16931 (the semantic core); also the Factur-X EN 16931 profile.
    En16931,
    /// PEPPOL BIS Billing 3.0.
    PeppolBis3,
    /// XRechnung 3.0 (German CIUS of EN 16931).
    XRechnung30,
}

pub const EN16931_ID: &str = "urn:cen.eu:en16931:2017";
pub const PEPPOL_BIS3_CUSTOMIZATION: &str =
    "urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0";
pub const XRECHNUNG_30_CUSTOMIZATION: &str =
    "urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0";
pub const PEPPOL_BIS3_PROFILE: &str = "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0";

impl Profile {
    /// The `cbc:CustomizationID` (UBL) / guideline parameter ID (CII).
    pub fn customization_id(self) -> &'static str {
        match self {
            Profile::En16931 => EN16931_ID,
            Profile::PeppolBis3 => PEPPOL_BIS3_CUSTOMIZATION,
            Profile::XRechnung30 => XRECHNUNG_30_CUSTOMIZATION,
        }
    }

    /// The `cbc:ProfileID` (business process), where the profile defines one.
    pub fn profile_id(self) -> Option<&'static str> {
        match self {
            Profile::En16931 => None,
            Profile::PeppolBis3 | Profile::XRechnung30 => Some(PEPPOL_BIS3_PROFILE),
        }
    }

    /// XRechnung mandates `cbc:BuyerReference` (the Leitweg-ID) — we surface the
    /// customer's external id there. PEPPOL recommends it too.
    pub fn wants_buyer_reference(self) -> bool {
        matches!(self, Profile::XRechnung30 | Profile::PeppolBis3)
    }
}

// ---- Amounts --------------------------------------------------------------

/// Format an integer-cents amount into its main-currency-unit decimal string
/// (e.g. `39060` USD → `"390.60"`), rounded half-up to the currency exponent.
pub fn amount(cents: i64, currency: &Currency) -> String {
    amount_dec(Decimal::from(cents), currency)
}

/// Format a precise-cents [`Decimal`] into the main-unit decimal string.
pub fn amount_dec(cents: Decimal, currency: &Currency) -> String {
    let v = cents / currency.subunit_to_unit_decimal();
    let v = round_half_up(v, currency.exponent());
    let prec = currency.exponent() as usize;
    format!("{v:.prec$}")
}

/// Format a percentage like Lago's `%.2f` (no `%` sign in the XML value).
pub fn percent(rate: f64) -> String {
    format!("{rate:.2}")
}

// ---- Tax grouping ---------------------------------------------------------

/// One VAT breakdown line: a category, its rate, the taxable basis and the tax,
/// both in precise cents. Mirrors the tuple Lago's `taxes` block yields.
#[derive(Debug, Clone)]
pub struct TaxGroup {
    pub category: &'static str,
    pub rate: f64,
    pub basis_cents: Decimal,
    pub tax_cents: Decimal,
}

/// UNCL 5305 category for a `(type, rate)` pair. `O` (out of scope) for credit
/// fees/invoices, `Z` (zero-rated) at 0%, otherwise `S` (standard rate).
pub fn tax_category(type_is_credit: bool, rate: f64) -> &'static str {
    if type_is_credit {
        O_CATEGORY
    } else if rate == 0.0 {
        Z_CATEGORY
    } else {
        S_CATEGORY
    }
}

fn f64_key(rate: f64) -> i64 {
    // Stable, ordered key for grouping/sorting float tax rates (2 dp precision).
    (rate * 100.0).round() as i64
}

/// Total allowances applied at invoice level (coupons + progressive credit).
pub fn invoice_allowances_cents(coupons: i64, progressive_credit: i64) -> Decimal {
    Decimal::from(coupons) + Decimal::from(progressive_credit)
}

/// Per-tax-rate allowance apportionment — faithful port of Lago's
/// `allowances_per_tax_rate`. Returns `(rate_key -> allowance_cents)`.
pub fn allowances_per_tax_rate(fees: &[Fee], allowances: Decimal) -> Vec<(f64, Decimal)> {
    let fees_total: Decimal = fees.iter().map(|f| f.precise_amount_cents).sum();
    let groups = group_by_rate(fees);
    groups
        .into_iter()
        .map(|(rate, group)| {
            let total_amount: Decimal = group.iter().map(|f| f.precise_amount_cents).sum();
            if rate > 0.0 {
                let total_taxes: Decimal = group.iter().map(|f| f.taxes_precise_amount_cents).sum();
                let charged = total_taxes * Decimal::from(100)
                    / Decimal::try_from(rate).unwrap_or(Decimal::ONE);
                (rate, total_amount - charged)
            } else {
                let proportion = if fees_total.is_zero() {
                    Decimal::ZERO
                } else {
                    total_amount / fees_total
                };
                (rate, proportion * allowances)
            }
        })
        .collect()
}

/// Group fees by tax rate, ascending (deterministic ordering).
fn group_by_rate(fees: &[Fee]) -> Vec<(f64, Vec<&Fee>)> {
    let mut keys: Vec<i64> = Vec::new();
    let mut buckets: Vec<(f64, Vec<&Fee>)> = Vec::new();
    for fee in fees {
        let k = f64_key(fee.taxes_rate);
        match keys.iter().position(|&x| x == k) {
            Some(idx) => buckets[idx].1.push(fee),
            None => {
                keys.push(k);
                buckets.push((fee.taxes_rate, vec![fee]));
            }
        }
    }
    buckets.sort_by_key(|(rate, _)| f64_key(*rate));
    buckets
}

/// Compute the VAT breakdown from an invoice's fees — port of Lago's `taxes`.
pub fn invoice_tax_groups(
    fees: &[Fee],
    invoice_type: InvoiceType,
    allowances: Decimal,
) -> Vec<TaxGroup> {
    let per_rate = allowances_per_tax_rate(fees, allowances);
    let type_is_credit = matches!(invoice_type, InvoiceType::Credit);
    group_by_rate(fees)
        .into_iter()
        .map(|(rate, group)| {
            let total_taxes: Decimal = group.iter().map(|f| f.taxes_precise_amount_cents).sum();
            let basis = if rate > 0.0 {
                total_taxes * Decimal::from(100) / Decimal::try_from(rate).unwrap_or(Decimal::ONE)
            } else {
                let total_amount: Decimal = group.iter().map(|f| f.precise_amount_cents).sum();
                let allowance = per_rate
                    .iter()
                    .find(|(r, _)| f64_key(*r) == f64_key(rate))
                    .map(|(_, a)| *a)
                    .unwrap_or(Decimal::ZERO);
                total_amount - allowance
            };
            TaxGroup {
                category: tax_category(type_is_credit, rate),
                rate,
                basis_cents: basis,
                tax_cents: total_taxes,
            }
        })
        .collect()
}

/// A human item name for a fee (Rago fees carry no `invoice_name`).
pub fn fee_item_name(fee: &Fee) -> String {
    use rago_core::enums::FeeType;
    match fee.fee_type {
        Some(FeeType::Subscription) => "Subscription".to_string(),
        Some(FeeType::Charge) => "Usage charge".to_string(),
        Some(FeeType::AddOn) => "Add-on".to_string(),
        Some(FeeType::Credit) => "Credit".to_string(),
        Some(FeeType::Commitment) => "Commitment".to_string(),
        Some(FeeType::FixedCharge) => "Fixed charge".to_string(),
        Some(FeeType::Product) => "Product".to_string(),
        None => "Fee".to_string(),
    }
}

/// A longer description for a fee line.
pub fn fee_description(fee: &Fee) -> String {
    use rago_core::enums::FeeType;
    match fee.fee_type {
        Some(FeeType::Charge) => format!("Usage charge ({} units)", fee.units.normalize()),
        _ => fee_item_name(fee),
    }
}

/// UNTDID 1001 type code for an invoice.
pub fn invoice_type_code(invoice_type: InvoiceType) -> i32 {
    match invoice_type {
        InvoiceType::Credit => PREPAID_INVOICE,
        _ => COMMERCIAL_INVOICE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_formats_to_currency_exponent() {
        let usd = Currency::new("USD");
        assert_eq!(amount(39060, &usd), "390.60");
        assert_eq!(amount(31000, &usd), "310.00");
        let jpy = Currency::new("JPY");
        assert_eq!(amount(500, &jpy), "500");
    }

    #[test]
    fn profile_ids_are_the_published_urns() {
        assert_eq!(
            Profile::En16931.customization_id(),
            "urn:cen.eu:en16931:2017"
        );
        assert!(Profile::PeppolBis3
            .customization_id()
            .contains("peppol.eu:2017:poacc:billing:3.0"));
        assert_eq!(Profile::PeppolBis3.profile_id(), Some(PEPPOL_BIS3_PROFILE));
        assert!(Profile::XRechnung30
            .customization_id()
            .contains("xrechnung_3.0"));
    }
}

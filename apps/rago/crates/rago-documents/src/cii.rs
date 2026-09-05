//! UN/CEFACT Cross-Industry Invoice (CII) D16B serialization — the XML syntax
//! behind Factur-X / ZUGFeRD (embedded in a PDF/A-3) and the CII flavour of
//! XRechnung. Field mapping follows Lago `app/serializers/e_invoices/cii/*`
//! and `.../invoices/cii/builder.rb`.

use rago_core::enums::InvoiceType;
use rago_core::models::{CreditNote, Customer, Fee, Invoice, Organization};
use rago_core::money::Currency;
use rust_decimal::Decimal;

use crate::einvoice::*;
use crate::xml::Xml;

fn root_ns() -> Vec<(&'static str, &'static str)> {
    vec![
        ("xmlns:xs", XS_NS),
        ("xmlns:rsm", CII_RSM_NS),
        ("xmlns:qdt", CII_QDT_NS),
        ("xmlns:ram", CII_RAM_NS),
        ("xmlns:udt", CII_UDT_NS),
    ]
}

fn cii_date(d: chrono::NaiveDate) -> String {
    d.format("%Y%m%d").to_string()
}

fn date_time_string(x: &mut Xml, date: chrono::NaiveDate) {
    x.leaf("udt:DateTimeString", &[("format", "102")], &cii_date(date));
}

fn context(x: &mut Xml, profile: Profile) {
    x.comment("Exchange Document Context");
    x.element("rsm:ExchangedDocumentContext", &[], |x| {
        x.element("ram:GuidelineSpecifiedDocumentContextParameter", &[], |x| {
            x.text("ram:ID", profile.customization_id());
        });
    });
}

fn header(x: &mut Xml, number: &str, type_code: i32, issue: chrono::NaiveDate, notes: &[String]) {
    x.comment("Exchange Document Header");
    x.element("rsm:ExchangedDocument", &[], |x| {
        x.text("ram:ID", number);
        x.text("ram:TypeCode", &type_code.to_string());
        x.element("ram:IssueDateTime", &[], |x| date_time_string(x, issue));
        for note in notes {
            x.element("ram:IncludedNote", &[], |x| x.text("ram:Content", note));
        }
    });
}

fn trade_agreement(
    x: &mut Xml,
    supplier: &Organization,
    customer: &Customer,
    tax_registration: bool,
) {
    let cust_name = customer.name.as_deref().unwrap_or(&customer.external_id);
    x.comment("Applicable Header Trade Agreement");
    x.element("ram:ApplicableHeaderTradeAgreement", &[], |x| {
        x.element("ram:SellerTradeParty", &[], |x| {
            x.text("ram:ID", &supplier.slug);
            x.text("ram:Name", &supplier.name);
            x.element("ram:PostalTradeAddress", &[], |x| {
                if let Some(country) = supplier.country.as_deref() {
                    x.text("ram:CountryID", country);
                }
            });
            if tax_registration {
                if let Some(tin) = supplier.tax_identification_number.as_deref() {
                    x.element("ram:SpecifiedTaxRegistration", &[], |x| {
                        x.leaf("ram:ID", &[("schemeID", "VA")], tin);
                    });
                }
            }
        });
        x.element("ram:BuyerTradeParty", &[], |x| {
            x.text("ram:Name", cust_name);
            x.element("ram:PostalTradeAddress", &[], |x| {
                if let Some(country) = customer.country.as_deref() {
                    x.text("ram:CountryID", country);
                }
            });
        });
    });
}

fn trade_delivery(x: &mut Xml, date: chrono::NaiveDate) {
    x.comment("Applicable Header Trade Delivery");
    x.element("ram:ApplicableHeaderTradeDelivery", &[], |x| {
        x.element("ram:ActualDeliverySupplyChainEvent", &[], |x| {
            x.element("ram:OccurrenceDateTime", &[], |x| date_time_string(x, date));
        });
    });
}

fn applicable_trade_tax(x: &mut Xml, g: &TaxGroup, currency: &Currency) {
    x.comment(&format!("Tax Information {} {}", percent(g.rate), VAT));
    x.element("ram:ApplicableTradeTax", &[], |x| {
        x.text("ram:CalculatedAmount", &amount_dec(g.tax_cents, currency));
        x.text("ram:TypeCode", VAT);
        x.text("ram:BasisAmount", &amount_dec(g.basis_cents, currency));
        x.text("ram:CategoryCode", g.category);
        if g.category == O_CATEGORY {
            x.text("ram:ExemptionReasonCode", O_VAT_EXEMPTION);
        } else {
            x.text("ram:RateApplicablePercent", &percent(g.rate));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn monetary_summation(
    x: &mut Xml,
    currency: &Currency,
    line_total: Decimal,
    charges: Decimal,
    allowances: Decimal,
    tax_basis: Decimal,
    tax: Decimal,
    grand_total: Decimal,
    prepaid: Option<Decimal>,
    due_payable: Decimal,
) {
    let cur = currency.code();
    let a = |v: Decimal| amount_dec(v, currency);
    x.comment("Monetary Summation");
    x.element(
        "ram:SpecifiedTradeSettlementHeaderMonetarySummation",
        &[],
        |x| {
            x.text("ram:LineTotalAmount", &a(line_total));
            x.text("ram:ChargeTotalAmount", &a(charges));
            x.text("ram:AllowanceTotalAmount", &a(allowances));
            x.text("ram:TaxBasisTotalAmount", &a(tax_basis));
            x.leaf("ram:TaxTotalAmount", &[("currencyID", &cur)], &a(tax));
            x.text("ram:GrandTotalAmount", &a(grand_total));
            if let Some(p) = prepaid {
                x.text("ram:TotalPrepaidAmount", &a(p));
            }
            x.text("ram:DuePayableAmount", &a(due_payable));
        },
    );
}

struct CiiLine {
    line_id: usize,
    name: String,
    description: String,
    charge_amount: Decimal,
    quantity: Decimal,
    category: &'static str,
    rate: f64,
    line_total: Decimal,
}

fn line_item(x: &mut Xml, d: &CiiLine, currency: &Currency) {
    x.comment(&format!("Line Item {}: {}", d.line_id, d.description));
    x.element("ram:IncludedSupplyChainTradeLineItem", &[], |x| {
        x.element("ram:AssociatedDocumentLineDocument", &[], |x| {
            x.text("ram:LineID", &d.line_id.to_string());
        });
        x.element("ram:SpecifiedTradeProduct", &[], |x| {
            x.text("ram:Name", &d.name);
            x.text("ram:Description", &d.description);
        });
        x.element("ram:SpecifiedLineTradeAgreement", &[], |x| {
            x.element("ram:NetPriceProductTradePrice", &[], |x| {
                x.text("ram:ChargeAmount", &d.charge_amount.normalize().to_string());
            });
        });
        x.element("ram:SpecifiedLineTradeDelivery", &[], |x| {
            x.leaf(
                "ram:BilledQuantity",
                &[("unitCode", UNIT_CODE)],
                &d.quantity.normalize().to_string(),
            );
        });
        x.element("ram:SpecifiedLineTradeSettlement", &[], |x| {
            x.element("ram:ApplicableTradeTax", &[], |x| {
                x.text("ram:TypeCode", VAT);
                x.text("ram:CategoryCode", d.category);
                if d.category != O_CATEGORY {
                    x.text("ram:RateApplicablePercent", &percent(d.rate));
                }
            });
            x.element(
                "ram:SpecifiedTradeSettlementLineMonetarySummation",
                &[],
                |x| {
                    x.text("ram:LineTotalAmount", &amount_dec(d.line_total, currency));
                },
            );
        });
    });
}

fn payment_means(x: &mut Xml, info: &str) {
    x.comment("Payment Means");
    x.element("ram:SpecifiedTradeSettlementPaymentMeans", &[], |x| {
        x.text("ram:TypeCode", &STANDARD_PAYMENT.to_string());
        x.text("ram:Information", info);
    });
}

fn payment_terms(x: &mut Xml, description: &str, due: chrono::NaiveDate) {
    x.comment("Payment Terms");
    x.element("ram:SpecifiedTradePaymentTerms", &[], |x| {
        x.text("ram:Description", description);
        x.element("ram:DueDateDateTime", &[], |x| date_time_string(x, due));
    });
}

/// Serialize a Rago invoice as a UN/CEFACT Cross-Industry Invoice (CII) — the
/// Factur-X / ZUGFeRD (and CII XRechnung) XML syntax — under [`Profile`].
pub fn cii_invoice_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
    profile: Profile,
) -> String {
    let currency = invoice.currency();
    let issue = invoice
        .issuing_date
        .unwrap_or(invoice.created_at.date_naive());
    let allowances = invoice_allowances_cents(
        invoice.coupons_amount_cents,
        invoice.progressive_billing_credit_amount_cents,
    );
    let groups = invoice_tax_groups(fees, invoice.invoice_type, allowances);
    let tax_registration = !matches!(invoice.invoice_type, InvoiceType::Credit);
    let notes = vec![format!("Invoice ID: {}", invoice.id)];

    let mut x = Xml::new();
    x.element("rsm:CrossIndustryInvoice", &root_ns(), |x| {
        context(x, profile);
        header(
            x,
            &invoice.number,
            invoice_type_code(invoice.invoice_type),
            issue,
            &notes,
        );

        x.comment("Supply Chain Trade Transaction");
        x.element("rsm:SupplyChainTradeTransaction", &[], |x| {
            for (i, fee) in fees.iter().enumerate() {
                let is_credit_fee = fee
                    .fee_type
                    .map(|t| t.as_str() == "credit")
                    .unwrap_or(false);
                let category = tax_category(is_credit_fee, fee.taxes_rate);
                let d = CiiLine {
                    line_id: i + 1,
                    name: fee_item_name(fee),
                    description: fee_description(fee),
                    charge_amount: fee.precise_unit_amount,
                    quantity: fee.units,
                    category,
                    rate: fee.taxes_rate,
                    line_total: Decimal::from(fee.amount_cents),
                };
                line_item(x, &d, &fee.currency());
            }

            trade_agreement(x, supplier, customer, tax_registration);
            trade_delivery(x, issue);

            x.comment("Applicable Header Trade Settlement");
            x.element("ram:ApplicableHeaderTradeSettlement", &[], |x| {
                x.text("ram:InvoiceCurrencyCode", &currency.code());
                payment_means(x, "Standard payment");
                for g in &groups {
                    applicable_trade_tax(x, g, &currency);
                }
                payment_terms(x, "Payment terms as per agreement", issue);
                let prepaid = Decimal::from(invoice.prepaid_credit_amount_cents)
                    + Decimal::from(invoice.credit_notes_amount_cents);
                monetary_summation(
                    x,
                    &currency,
                    Decimal::from(invoice.fees_amount_cents),
                    Decimal::ZERO,
                    allowances,
                    Decimal::from(invoice.sub_total_excluding_taxes_amount_cents),
                    Decimal::from(invoice.taxes_amount_cents),
                    Decimal::from(invoice.sub_total_including_taxes_amount_cents),
                    Some(prepaid),
                    Decimal::from(invoice.total_amount_cents),
                );
            });
        });
    });
    x.finish()
}

/// Serialize a Rago credit note as a CII document (type code 381), negative
/// amounts, mirroring Lago's credit-note CII builder.
pub fn cii_credit_note_xml(
    cn: &CreditNote,
    supplier: &Organization,
    customer: &Customer,
    invoice_number: &str,
    profile: Profile,
) -> String {
    let currency = Currency::new(&cn.total_amount_currency);
    let issue = cn.issuing_date;
    let created = cn.created_at.date_naive();
    let items_sum: Decimal = cn.items.iter().map(|it| it.precise_amount_cents).sum();
    let sub_excl = items_sum - cn.precise_coupons_adjustment_amount_cents;
    let notes = vec![
        format!("Credit Note ID: {}", cn.id),
        format!("Original Invoice: {invoice_number}"),
    ];

    let mut x = Xml::new();
    x.element("rsm:CrossIndustryInvoice", &root_ns(), |x| {
        context(x, profile);
        header(x, &cn.number, CREDIT_NOTE, issue, &notes);

        x.comment("Supply Chain Trade Transaction");
        x.element("rsm:SupplyChainTradeTransaction", &[], |x| {
            for (i, item) in cn.items.iter().enumerate() {
                let d = CiiLine {
                    line_id: i + 1,
                    name: "Credited amount".to_string(),
                    description: format!("Credit against invoice {invoice_number}"),
                    charge_amount: item.precise_amount_cents / currency.subunit_to_unit_decimal(),
                    quantity: Decimal::NEGATIVE_ONE,
                    category: tax_category(false, cn.taxes_rate),
                    rate: cn.taxes_rate,
                    line_total: -item.precise_amount_cents,
                };
                line_item(x, &d, &currency);
            }

            trade_agreement(x, supplier, customer, true);
            trade_delivery(x, created);

            x.comment("Applicable Header Trade Settlement");
            x.element("ram:ApplicableHeaderTradeSettlement", &[], |x| {
                x.text("ram:InvoiceCurrencyCode", &currency.code());
                payment_means(x, "Credit note settlement");
                let basis = Decimal::from(cn.total_amount_cents) - cn.precise_taxes_amount_cents;
                let g = TaxGroup {
                    category: tax_category(false, cn.taxes_rate),
                    rate: cn.taxes_rate,
                    basis_cents: -basis,
                    tax_cents: -cn.precise_taxes_amount_cents,
                };
                applicable_trade_tax(x, &g, &currency);
                payment_terms(x, "Credit note - immediate settlement", created);
                monetary_summation(
                    x,
                    &currency,
                    -items_sum,
                    cn.precise_coupons_adjustment_amount_cents,
                    Decimal::ZERO,
                    -sub_excl,
                    -cn.precise_taxes_amount_cents,
                    -(sub_excl + cn.precise_taxes_amount_cents),
                    None,
                    -Decimal::from(cn.total_amount_cents),
                );
            });
        });
    });
    x.finish()
}

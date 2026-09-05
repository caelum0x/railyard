//! OASIS UBL 2.1 serialization — Invoice-2, CreditNote-2 and the
//! ApplicationResponse-2 payment receipt. This is the syntax the PEPPOL BIS
//! Billing 3.0 and XRechnung (UBL) profiles ride on.
//!
//! Field mapping follows Lago `app/serializers/e_invoices/ubl/*` and
//! `.../invoices/ubl/builder.rb`; the profile IDs come from [`crate::einvoice`].

use rago_core::enums::InvoiceType;
use rago_core::models::{CreditNote, Customer, Fee, Invoice, Organization};
use rago_core::money::Currency;
use rust_decimal::Decimal;

use crate::einvoice::*;
use crate::xml::Xml;

fn ns_attrs(root_ns: &str) -> Vec<(&str, &str)> {
    vec![
        ("xmlns", root_ns),
        ("xmlns:cac", UBL_CAC_NS),
        ("xmlns:cbc", UBL_CBC_NS),
    ]
}

fn iso_date(d: Option<chrono::NaiveDate>, fallback: chrono::NaiveDate) -> String {
    d.unwrap_or(fallback).format("%Y-%m-%d").to_string()
}

fn customization(x: &mut Xml, profile: Profile) {
    x.comment("UBL Version and Customization");
    x.text("cbc:UBLVersionID", "2.1");
    x.text("cbc:CustomizationID", profile.customization_id());
    if let Some(p) = profile.profile_id() {
        x.text("cbc:ProfileID", p);
    }
}

/// UBL `cac:AccountingSupplierParty`. `tax_registration` gates the VAT scheme
/// (Lago omits it for prepaid-credit invoices).
fn supplier_party(x: &mut Xml, supplier: &Organization, tax_registration: bool) {
    x.comment("Supplier Party");
    x.element("cac:AccountingSupplierParty", &[], |x| {
        x.element("cac:Party", &[], |x| {
            if let Some(email) = supplier.email.as_deref() {
                x.leaf("cbc:EndpointID", &[("schemeID", "EM")], email);
            }
            x.element("cac:PostalAddress", &[], |x| {
                if let Some(country) = supplier.country.as_deref() {
                    x.element("cac:Country", &[], |x| {
                        x.text("cbc:IdentificationCode", country);
                    });
                }
            });
            if tax_registration {
                if let Some(tin) = supplier.tax_identification_number.as_deref() {
                    x.element("cac:PartyTaxScheme", &[], |x| {
                        x.text("cbc:CompanyID", tin);
                        x.element("cac:TaxScheme", &[], |x| x.text("cbc:ID", VAT));
                    });
                }
            }
            x.element("cac:PartyLegalEntity", &[], |x| {
                x.text("cbc:RegistrationName", &supplier.name);
                if let Some(tin) = supplier.tax_identification_number.as_deref() {
                    x.text("cbc:CompanyID", tin);
                }
            });
            x.element("cac:Contact", &[], |x| {
                x.text("cbc:Name", &supplier.name);
                if let Some(email) = supplier.email.as_deref() {
                    x.text("cbc:ElectronicMail", email);
                }
            });
        });
    });
}

/// UBL `cac:AccountingCustomerParty`.
fn customer_party(x: &mut Xml, customer: &Customer) {
    let name = customer.name.as_deref().unwrap_or(&customer.external_id);
    x.comment("Customer Party");
    x.element("cac:AccountingCustomerParty", &[], |x| {
        x.element("cac:Party", &[], |x| {
            if let Some(email) = customer.email.as_deref() {
                x.leaf("cbc:EndpointID", &[("schemeID", "EM")], email);
            }
            x.element("cac:PostalAddress", &[], |x| {
                if let Some(country) = customer.country.as_deref() {
                    x.element("cac:Country", &[], |x| {
                        x.text("cbc:IdentificationCode", country);
                    });
                }
            });
            x.element("cac:PartyLegalEntity", &[], |x| {
                x.text("cbc:RegistrationName", name);
            });
            x.element("cac:Contact", &[], |x| {
                x.text("cbc:Name", name);
                if let Some(email) = customer.email.as_deref() {
                    x.text("cbc:ElectronicMail", email);
                }
            });
        });
    });
}

fn tax_subtotal(x: &mut Xml, g: &TaxGroup, currency: &Currency) {
    let cur = currency.code();
    x.comment(&format!("Tax Information {} {}", percent(g.rate), VAT));
    x.element("cac:TaxSubtotal", &[], |x| {
        x.leaf(
            "cbc:TaxableAmount",
            &[("currencyID", &cur)],
            &amount_dec(g.basis_cents, currency),
        );
        x.leaf(
            "cbc:TaxAmount",
            &[("currencyID", &cur)],
            &amount_dec(g.tax_cents, currency),
        );
        x.element("cac:TaxCategory", &[], |x| {
            x.text("cbc:ID", g.category);
            if g.category == O_CATEGORY {
                x.text("cbc:TaxExemptionReasonCode", O_VAT_EXEMPTION);
                x.text("cbc:TaxExemptionReason", "Not subject to VAT");
            } else {
                x.text("cbc:Percent", &percent(g.rate));
            }
            x.element("cac:TaxScheme", &[], |x| x.text("cbc:ID", VAT));
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn monetary_total(
    x: &mut Xml,
    currency: &Currency,
    line_extension: Decimal,
    tax_exclusive: Decimal,
    tax_inclusive: Decimal,
    allowance_total: Decimal,
    charge_total: Decimal,
    prepaid: Decimal,
    payable: Decimal,
) {
    let cur = currency.code();
    let a = |v: Decimal| amount_dec(v, currency);
    x.comment("Legal Monetary Total");
    x.element("cac:LegalMonetaryTotal", &[], |x| {
        x.leaf(
            "cbc:LineExtensionAmount",
            &[("currencyID", &cur)],
            &a(line_extension),
        );
        x.leaf(
            "cbc:TaxExclusiveAmount",
            &[("currencyID", &cur)],
            &a(tax_exclusive),
        );
        x.leaf(
            "cbc:TaxInclusiveAmount",
            &[("currencyID", &cur)],
            &a(tax_inclusive),
        );
        x.leaf(
            "cbc:AllowanceTotalAmount",
            &[("currencyID", &cur)],
            &a(allowance_total),
        );
        x.leaf(
            "cbc:ChargeTotalAmount",
            &[("currencyID", &cur)],
            &a(charge_total),
        );
        x.leaf("cbc:PrepaidAmount", &[("currencyID", &cur)], &a(prepaid));
        x.leaf("cbc:PayableAmount", &[("currencyID", &cur)], &a(payable));
    });
}

struct LineData<'a> {
    tag: &'a str,
    quantity_tag: &'a str,
    line_id: usize,
    quantity: Decimal,
    line_extension: Decimal,
    item_name: String,
    category: &'static str,
    rate: f64,
    description: String,
    price_amount: Decimal,
}

fn line_item(x: &mut Xml, d: &LineData, currency: &Currency) {
    let cur = currency.code();
    let id = d.line_id.to_string();
    x.comment(&format!("Line Item {}: {}", d.line_id, d.description));
    x.element(d.tag, &[], |x| {
        x.text("cbc:ID", &id);
        x.leaf(
            d.quantity_tag,
            &[("unitCode", UNIT_CODE)],
            &d.quantity.normalize().to_string(),
        );
        x.leaf(
            "cbc:LineExtensionAmount",
            &[("currencyID", &cur)],
            &amount_dec(d.line_extension, currency),
        );
        x.element("cac:Item", &[], |x| {
            x.text("cbc:Name", &d.item_name);
            x.element("cac:ClassifiedTaxCategory", &[], |x| {
                x.text("cbc:ID", d.category);
                if d.category != O_CATEGORY {
                    x.text("cbc:Percent", &percent(d.rate));
                }
                x.element("cac:TaxScheme", &[], |x| x.text("cbc:ID", VAT));
            });
            x.element("cac:AdditionalItemProperty", &[], |x| {
                x.text("cbc:Name", "Description");
                x.text("cbc:Value", &d.description);
            });
        });
        x.element("cac:Price", &[], |x| {
            x.leaf(
                "cbc:PriceAmount",
                &[("currencyID", &cur)],
                &d.price_amount.normalize().to_string(),
            );
        });
    });
}

/// Serialize a Rago invoice as a UBL 2.1 `Invoice` under the given [`Profile`]
/// (EN 16931, PEPPOL BIS 3.0, or XRechnung 3.0).
pub fn ubl_invoice_xml(
    invoice: &Invoice,
    fees: &[Fee],
    supplier: &Organization,
    customer: &Customer,
    profile: Profile,
) -> String {
    let currency = invoice.currency();
    let cur = currency.code();
    let issue = iso_date(invoice.issuing_date, invoice.created_at.date_naive());
    let allowances = invoice_allowances_cents(
        invoice.coupons_amount_cents,
        invoice.progressive_billing_credit_amount_cents,
    );
    let groups = invoice_tax_groups(fees, invoice.invoice_type, allowances);
    let tax_registration = !matches!(invoice.invoice_type, InvoiceType::Credit);

    let mut x = Xml::new();
    x.element("Invoice", &ns_attrs(UBL_INVOICE_NS), |x| {
        customization(x, profile);

        // Header.
        x.comment("Invoice Header Information");
        x.text("cbc:ID", &invoice.number);
        x.text("cbc:IssueDate", &issue);
        x.text(
            "cbc:InvoiceTypeCode",
            &invoice_type_code(invoice.invoice_type).to_string(),
        );
        x.text("cbc:Note", &format!("Invoice ID: {}", invoice.id));
        x.text("cbc:DocumentCurrencyCode", &cur);
        if profile.wants_buyer_reference() {
            x.text("cbc:BuyerReference", &customer.external_id);
        }

        supplier_party(x, supplier, tax_registration);
        customer_party(x, customer);

        x.comment("Delivery Information");
        x.element("cac:Delivery", &[], |x| {
            x.text("cbc:ActualDeliveryDate", &issue);
        });

        x.comment("Payment Means");
        x.element("cac:PaymentMeans", &[], |x| {
            x.text("cbc:PaymentMeansCode", &STANDARD_PAYMENT.to_string());
        });

        x.comment("Payment Terms");
        x.element("cac:PaymentTerms", &[], |x| {
            x.text("cbc:Note", "Payment terms as per agreement");
        });

        // Tax total.
        x.comment("Tax Total Information");
        x.element("cac:TaxTotal", &[], |x| {
            x.leaf(
                "cbc:TaxAmount",
                &[("currencyID", &cur)],
                &amount(invoice.taxes_amount_cents, &currency),
            );
            for g in &groups {
                tax_subtotal(x, g, &currency);
            }
        });

        // Legal monetary total.
        let prepaid = Decimal::from(invoice.prepaid_credit_amount_cents)
            + Decimal::from(invoice.credit_notes_amount_cents);
        monetary_total(
            x,
            &currency,
            Decimal::from(invoice.fees_amount_cents),
            Decimal::from(invoice.sub_total_excluding_taxes_amount_cents),
            Decimal::from(invoice.sub_total_including_taxes_amount_cents),
            allowances,
            Decimal::ZERO,
            prepaid,
            Decimal::from(invoice.total_amount_cents),
        );

        // Lines.
        for (i, fee) in fees.iter().enumerate() {
            let is_credit_fee = fee
                .fee_type
                .map(|t| t.as_str() == "credit")
                .unwrap_or(false);
            let category = tax_category(is_credit_fee, fee.taxes_rate);
            let d = LineData {
                tag: "cac:InvoiceLine",
                quantity_tag: "cbc:InvoicedQuantity",
                line_id: i + 1,
                quantity: fee.units,
                line_extension: Decimal::from(fee.amount_cents),
                item_name: fee_item_name(fee),
                category,
                rate: fee.taxes_rate,
                description: fee_description(fee),
                price_amount: fee.precise_unit_amount,
            };
            line_item(x, &d, &fee.currency());
        }
    });
    x.finish()
}

/// Serialize a Rago credit note as a UBL 2.1 `CreditNote` (type code 381),
/// referencing the original invoice via `cac:BillingReference`. Amounts are
/// negative, mirroring Lago's credit-note builder.
pub fn ubl_credit_note_xml(
    cn: &CreditNote,
    supplier: &Organization,
    customer: &Customer,
    invoice_number: &str,
    invoice_issue_date: chrono::NaiveDate,
    profile: Profile,
) -> String {
    let currency = Currency::new(&cn.total_amount_currency);
    let cur = currency.code();
    let issue = cn.issuing_date.format("%Y-%m-%d").to_string();

    // Sub-totals (Lago: excl = items - coupons_adjustment; incl = excl + taxes).
    let items_sum: Decimal = cn.items.iter().map(|it| it.precise_amount_cents).sum();
    let sub_excl = items_sum - cn.precise_coupons_adjustment_amount_cents;
    let sub_incl = sub_excl + cn.precise_taxes_amount_cents;

    let mut x = Xml::new();
    x.element("CreditNote", &ns_attrs(UBL_CREDIT_NOTE_NS), |x| {
        customization(x, profile);

        x.comment("Credit Note Header Information");
        x.text("cbc:ID", &cn.number);
        x.text("cbc:IssueDate", &issue);
        x.text("cbc:CreditNoteTypeCode", &CREDIT_NOTE.to_string());
        x.text("cbc:Note", &format!("Credit Note ID: {}", cn.id));
        x.text("cbc:Note", &format!("Original Invoice: {invoice_number}"));
        x.text("cbc:DocumentCurrencyCode", &cur);
        if profile.wants_buyer_reference() {
            x.text("cbc:BuyerReference", &customer.external_id);
        }

        x.comment("Reference to Original Invoice");
        x.element("cac:BillingReference", &[], |x| {
            x.element("cac:InvoiceDocumentReference", &[], |x| {
                x.text("cbc:ID", invoice_number);
                x.text(
                    "cbc:IssueDate",
                    &invoice_issue_date.format("%Y-%m-%d").to_string(),
                );
            });
        });

        supplier_party(x, supplier, true);
        customer_party(x, customer);

        x.comment("Payment Means");
        x.element("cac:PaymentMeans", &[], |x| {
            x.text("cbc:PaymentMeansCode", &STANDARD_PAYMENT.to_string());
        });
        x.comment("Payment Terms");
        x.element("cac:PaymentTerms", &[], |x| {
            x.text("cbc:Note", "Credit note - immediate settlement");
        });

        // Tax total (negative).
        x.comment("Tax Total Information");
        x.element("cac:TaxTotal", &[], |x| {
            x.leaf(
                "cbc:TaxAmount",
                &[("currencyID", &cur)],
                &amount_dec(-cn.precise_taxes_amount_cents, &currency),
            );
            for tax in credit_note_tax_groups(cn) {
                tax_subtotal(x, &tax, &currency);
            }
        });

        // Monetary total (negative).
        monetary_total(
            x,
            &currency,
            -items_sum,
            -sub_excl,
            -sub_incl,
            Decimal::ZERO,
            cn.precise_coupons_adjustment_amount_cents,
            Decimal::ZERO,
            -Decimal::from(cn.total_amount_cents),
        );

        // Lines (one per credit-note item).
        for (i, item) in cn.items.iter().enumerate() {
            let d = LineData {
                tag: "cac:CreditNoteLine",
                quantity_tag: "cbc:CreditedQuantity",
                line_id: i + 1,
                quantity: Decimal::ONE,
                line_extension: -item.precise_amount_cents,
                item_name: "Credited amount".to_string(),
                category: tax_category(false, cn.taxes_rate),
                rate: cn.taxes_rate,
                description: format!("Credit against invoice {invoice_number}"),
                price_amount: item.precise_amount_cents / currency.subunit_to_unit_decimal(),
            };
            line_item(x, &d, &currency);
        }
    });
    x.finish()
}

/// Credit-note VAT breakdown from `applied_taxes` (falling back to the single
/// rollup rate when none are attached).
fn credit_note_tax_groups(cn: &CreditNote) -> Vec<TaxGroup> {
    if !cn.applied_taxes.is_empty() {
        cn.applied_taxes
            .iter()
            .map(|t| TaxGroup {
                category: tax_category(false, t.tax_rate),
                rate: t.tax_rate,
                basis_cents: -Decimal::from(t.base_amount_cents),
                tax_cents: -Decimal::from(t.amount_cents),
            })
            .collect()
    } else {
        let basis = Decimal::from(cn.total_amount_cents) - cn.precise_taxes_amount_cents;
        vec![TaxGroup {
            category: tax_category(false, cn.taxes_rate),
            rate: cn.taxes_rate,
            basis_cents: -basis,
            tax_cents: -cn.precise_taxes_amount_cents,
        }]
    }
}

/// Serialize a UBL 2.1 `ApplicationResponse` payment receipt for a paid
/// invoice (type code 202), mirroring Lago's payments UBL builder.
pub fn ubl_receipt_xml(
    invoice: &Invoice,
    supplier: &Organization,
    customer: &Customer,
    receipt_number: &str,
    paid_date: chrono::NaiveDate,
) -> String {
    let currency = invoice.currency();
    let paid = paid_date.format("%Y-%m-%d").to_string();
    let note = format!(
        "Receipt {} for payment of {} {} for invoice {}",
        receipt_number,
        currency.code(),
        amount(invoice.total_amount_cents, &currency),
        invoice.number
    );
    let cust_name = customer.name.as_deref().unwrap_or(&customer.external_id);

    let mut x = Xml::new();
    x.element("ApplicationResponse", &ns_attrs(UBL_RESPONSE_NS), |x| {
        x.comment("UBL Version and Customization");
        x.text("cbc:UBLVersionID", "2.1");
        x.text(
            "cbc:CustomizationID",
            "urn:oasis:names:specification:ubl:xpath:ApplicationResponse-2.4",
        );
        x.text("cbc:ProfileID", UBL_RESPONSE_NS);
        x.text("cbc:ID", receipt_number);
        x.text("cbc:IssueDate", &paid);
        x.text("cbc:Note", &note);

        x.comment("Sender Party");
        x.element("cac:SenderParty", &[], |x| {
            x.element("cac:PartyIdentification", &[], |x| {
                x.text("cbc:ID", &supplier.slug)
            });
            x.element("cac:PartyName", &[], |x| x.text("cbc:Name", &supplier.name));
            x.element("cac:PostalAddress", &[], |x| {
                if let Some(country) = supplier.country.as_deref() {
                    x.element("cac:Country", &[], |x| {
                        x.text("cbc:IdentificationCode", country)
                    });
                }
            });
        });

        x.comment("Receiver Party");
        x.element("cac:ReceiverParty", &[], |x| {
            x.element("cac:PartyName", &[], |x| x.text("cbc:Name", cust_name));
            x.element("cac:PostalAddress", &[], |x| {
                if let Some(country) = customer.country.as_deref() {
                    x.element("cac:Country", &[], |x| {
                        x.text("cbc:IdentificationCode", country)
                    });
                }
            });
        });

        x.comment("Document Response");
        x.element("cac:DocumentResponse", &[], |x| {
            x.element("cac:Response", &[], |x| {
                x.text("cbc:ResponseCode", PAID_RESPONSE);
                x.text("cbc:Description", &note);
                x.text("cbc:EffectiveDate", &paid);
            });
            x.element("cac:DocumentReference", &[], |x| {
                x.text("cbc:ID", &invoice.number);
                x.text(
                    "cbc:IssueDate",
                    &iso_date(invoice.issuing_date, invoice.created_at.date_naive()),
                );
                x.text(
                    "cbc:DocumentTypeCode",
                    &invoice_type_code(invoice.invoice_type).to_string(),
                );
                x.text("cbc:DocumentType", "Invoice");
                x.text(
                    "cbc:DocumentDescription",
                    &format!("Invoice ID: {}", invoice.id),
                );
            });
        });
    });
    x.finish()
}

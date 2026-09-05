//! GraphQL object types for slice-2 admin resources: billing entities, invoice
//! custom sections, usage-monitoring alerts, fixed charges, payments, features,
//! and the payment-URL payload. Faithful ports of the corresponding
//! `Types::*::Object` in Lago's schema.

use async_graphql::{SimpleObject, ID};
use rago_core::enums::ChargeModel;

/// `Types::BillingEntities::Object`.
#[derive(SimpleObject)]
pub struct BillingEntity {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub email: Option<String>,
    pub country: Option<String>,
    pub default_currency: String,
    pub timezone: String,
    pub applied_dunning_campaign_id: Option<ID>,
}

impl From<rago_db::BillingEntityRow> for BillingEntity {
    fn from(r: rago_db::BillingEntityRow) -> Self {
        Self {
            id: r.id.into(),
            code: r.code,
            name: r.name,
            email: r.email,
            country: r.country,
            default_currency: r.default_currency,
            timezone: r.timezone,
            applied_dunning_campaign_id: r.applied_dunning_campaign_id.map(Into::into),
        }
    }
}

/// `Types::InvoiceCustomSections::Object`.
#[derive(SimpleObject)]
pub struct InvoiceCustomSection {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub details: Option<String>,
    pub display_name: Option<String>,
}

impl From<rago_db::InvoiceCustomSectionRow> for InvoiceCustomSection {
    fn from(r: rago_db::InvoiceCustomSectionRow) -> Self {
        Self {
            id: r.id.into(),
            code: r.code,
            name: r.name,
            description: r.description,
            details: r.details,
            display_name: r.display_name,
        }
    }
}

/// `Types::UsageMonitoring::Alerts::ThresholdObject`.
#[derive(SimpleObject)]
pub struct AlertThreshold {
    pub code: Option<String>,
    pub value: String,
    pub recurring: bool,
}

/// `Types::UsageMonitoring::Alerts::Object`.
#[derive(SimpleObject)]
pub struct Alert {
    pub id: ID,
    pub alert_type: String,
    pub code: String,
    pub name: Option<String>,
    pub alertable_type: String,
    pub lago_alertable_id: ID,
    pub thresholds: Vec<AlertThreshold>,
}

impl Alert {
    pub fn build(row: rago_db::AlertRow, thresholds: Vec<rago_db::AlertThresholdRow>) -> Self {
        Self {
            id: row.id.into(),
            alert_type: row.alert_type,
            code: row.code,
            name: row.name,
            alertable_type: row.alertable_type,
            lago_alertable_id: row.alertable_id.into(),
            thresholds: thresholds
                .into_iter()
                .map(|(code, value, recurring)| AlertThreshold { code, value: value.to_string(), recurring })
                .collect(),
        }
    }
}

/// `Types::FixedCharges::Object`.
#[derive(SimpleObject)]
pub struct FixedCharge {
    pub id: ID,
    pub lago_plan_id: ID,
    pub lago_add_on_id: Option<ID>,
    pub charge_model: String,
    pub units: String,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoice_display_name: Option<String>,
}

impl From<rago_db::FixedChargeRow> for FixedCharge {
    fn from(r: rago_db::FixedChargeRow) -> Self {
        let charge_model = ChargeModel::try_from(r.charge_model).map(|m| m.as_str()).unwrap_or("standard").to_string();
        Self {
            id: r.id.into(),
            lago_plan_id: r.plan_id.into(),
            lago_add_on_id: r.add_on_id.map(Into::into),
            charge_model,
            units: r.units.to_string(),
            pay_in_advance: r.pay_in_advance,
            prorated: r.prorated,
            invoice_display_name: r.invoice_display_name,
        }
    }
}

/// `Types::PaymentProviders::Payment` (minimal) — a payment record.
#[derive(SimpleObject)]
pub struct Payment {
    pub id: ID,
    pub provider: String,
    pub status: String,
    pub amount_cents: i64,
    pub amount_currency: String,
}

impl From<rago_db::PaymentRow> for Payment {
    fn from(r: rago_db::PaymentRow) -> Self {
        let (id, provider, status, amount_cents, amount_currency) = r;
        Self { id: id.into(), provider, status, amount_cents, amount_currency }
    }
}

/// `Types::Entitlement::FeatureObject`.
#[derive(SimpleObject)]
pub struct Feature {
    pub id: ID,
    pub code: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

impl From<rago_db::FeatureRow> for Feature {
    fn from(r: rago_db::FeatureRow) -> Self {
        let (id, code, name, description) = r;
        Self { id: id.into(), code, name, description }
    }
}

/// `GeneratePaymentUrl` payload.
#[derive(SimpleObject)]
pub struct PaymentUrl {
    pub payment_url: Option<String>,
}

/// `Types::Entitlement::SubscriptionEntitlementObject` (flattened) — a feature
/// entitlement resolved for a plan or subscription, with an optional privilege
/// value.
#[derive(SimpleObject)]
pub struct Entitlement {
    pub feature_code: String,
    pub feature_name: Option<String>,
    pub privilege_code: Option<String>,
    pub value: Option<String>,
}

impl From<(String, Option<String>, Option<String>, Option<String>)> for Entitlement {
    fn from(t: (String, Option<String>, Option<String>, Option<String>)) -> Self {
        let (feature_code, feature_name, privilege_code, value) = t;
        Self { feature_code, feature_name, privilege_code, value }
    }
}

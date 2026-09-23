//! GraphQL object types for slice-3 admin resources: payment providers,
//! provider-customer connections, payment methods, payment requests, and the
//! integration mapping/customer/item objects. Faithful ports of the
//! corresponding `Types::PaymentProviders::*` / `Types::Integrations::*` objects.
//!
//! Provider config exposes a masked `secretRef` (e.g. `****4242`) — never the
//! raw credential.

use async_graphql::{SimpleObject, ID};

/// `Types::PaymentProviders::Object` (unified) — a provider configuration.
#[derive(SimpleObject)]
pub struct PaymentProvider {
    pub id: ID,
    pub provider_type: String,
    pub code: String,
    pub name: Option<String>,
    pub secret_ref: Option<String>,
    pub success_redirect_url: Option<String>,
    /// Masked inbound webhook signing secret (`****last4`), when stored.
    pub webhook_secret_ref: Option<String>,
    /// Provider settings (merchant account, 3DS flags, ...). Never secrets.
    pub settings: async_graphql::Json<serde_json::Value>,
}

impl From<rago_db::PaymentProviderRow> for PaymentProvider {
    fn from(r: rago_db::PaymentProviderRow) -> Self {
        Self {
            id: r.id.into(),
            provider_type: r.provider_type,
            code: r.code,
            name: r.name,
            secret_ref: r.secret_ref,
            success_redirect_url: r.success_redirect_url,
            webhook_secret_ref: r.webhook_secret_ref,
            settings: async_graphql::Json(r.settings),
        }
    }
}

/// `Types::PaymentProviderCustomers::Provider`.
#[derive(SimpleObject)]
pub struct PaymentProviderCustomer {
    pub id: ID,
    pub lago_customer_id: ID,
    pub provider_type: String,
    pub provider_customer_id: Option<String>,
    pub payment_provider_code: Option<String>,
    pub is_default: bool,
}

impl From<rago_db::PaymentProviderCustomerRow> for PaymentProviderCustomer {
    fn from(r: rago_db::PaymentProviderCustomerRow) -> Self {
        Self {
            id: r.id.into(),
            lago_customer_id: r.customer_id.into(),
            provider_type: r.provider,
            provider_customer_id: r.provider_customer_id,
            payment_provider_code: r.payment_provider_code,
            is_default: r.is_default,
        }
    }
}

/// `Types::PaymentMethods::Object`.
#[derive(SimpleObject)]
pub struct PaymentMethod {
    pub id: ID,
    pub lago_customer_id: ID,
    pub provider_method_id: Option<String>,
    pub is_default: bool,
}

impl From<rago_db::PaymentMethodRow> for PaymentMethod {
    fn from(r: rago_db::PaymentMethodRow) -> Self {
        let (id, customer_id, provider_method_id, is_default) = r;
        Self { id: id.into(), lago_customer_id: customer_id.into(), provider_method_id, is_default }
    }
}

/// `Types::PaymentRequests::Object`.
#[derive(SimpleObject)]
pub struct PaymentRequest {
    pub id: ID,
    pub external_customer_id: Option<String>,
    pub email: Option<String>,
    pub amount_cents: i64,
    pub amount_currency: Option<String>,
    pub payment_status: String,
}

impl From<rago_db::PaymentRequestRow> for PaymentRequest {
    fn from(r: rago_db::PaymentRequestRow) -> Self {
        let (id, external_customer_id, email, amount_cents, amount_currency, payment_status) = r;
        Self { id: id.into(), external_customer_id, email, amount_cents, amount_currency, payment_status }
    }
}

/// `Types::IntegrationCustomers::Object`.
#[derive(SimpleObject)]
pub struct IntegrationCustomer {
    pub id: ID,
    pub lago_integration_id: ID,
    pub lago_customer_id: ID,
    pub external_customer_id: Option<String>,
    pub code: Option<String>,
    pub is_default: bool,
}

impl From<rago_db::IntegrationCustomerRow> for IntegrationCustomer {
    fn from(r: rago_db::IntegrationCustomerRow) -> Self {
        Self {
            id: r.id.into(),
            lago_integration_id: r.integration_id.into(),
            lago_customer_id: r.customer_id.into(),
            external_customer_id: r.external_customer_id,
            code: r.code,
            is_default: r.is_default,
        }
    }
}

/// `Types::IntegrationCollectionMappings::Object`.
#[derive(SimpleObject)]
pub struct IntegrationCollectionMapping {
    pub id: ID,
    pub lago_integration_id: ID,
    pub mapping_type: String,
    pub external_id: Option<String>,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

impl From<rago_db::IntegrationCollectionMappingRow> for IntegrationCollectionMapping {
    fn from(r: rago_db::IntegrationCollectionMappingRow) -> Self {
        Self {
            id: r.id.into(),
            lago_integration_id: r.integration_id.into(),
            mapping_type: r.mapping_type,
            external_id: r.external_id,
            external_account_code: r.external_account_code,
            external_name: r.external_name,
        }
    }
}

/// `Types::IntegrationMappings::Object`.
#[derive(SimpleObject)]
pub struct IntegrationMapping {
    pub id: ID,
    pub lago_integration_id: ID,
    pub mappable_type: String,
    pub mappable_id: ID,
    pub external_id: String,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

impl From<rago_db::IntegrationMappingRow> for IntegrationMapping {
    fn from(r: rago_db::IntegrationMappingRow) -> Self {
        Self {
            id: r.id.into(),
            lago_integration_id: r.integration_connection_id.into(),
            mappable_type: r.mappable_type,
            mappable_id: r.mappable_id.into(),
            external_id: r.external_id,
            external_account_code: r.external_account_code,
            external_name: r.external_name,
        }
    }
}

/// `Types::IntegrationItems::Object`.
#[derive(SimpleObject)]
pub struct IntegrationItem {
    pub id: ID,
    pub external_id: String,
    pub item_type: Option<String>,
    pub external_name: Option<String>,
    pub external_account_code: Option<String>,
}

impl From<rago_db::IntegrationItemRow> for IntegrationItem {
    fn from(r: rago_db::IntegrationItemRow) -> Self {
        let (id, external_id, item_type, external_name, external_account_code) = r;
        Self { id: id.into(), external_id, item_type, external_name, external_account_code }
    }
}

/// `generateCheckoutUrl` / `GeneratePaymentUrl` checkout payload.
#[derive(SimpleObject)]
pub struct CheckoutUrl {
    pub checkout_url: Option<String>,
}

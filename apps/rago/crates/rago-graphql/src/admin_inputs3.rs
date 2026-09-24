//! Input objects for slice-3 admin mutations (payment providers + integrations
//! config). Secret-bearing fields (`secret_key`, `api_key`, `client_secret`,
//! `token_secret`, …) are captured only to derive a masked `secret_ref`; the raw
//! value is never persisted.

use async_graphql::{InputObject, ID};

// ---- payment providers -----------------------------------------------------

#[derive(InputObject)]
pub struct StripePaymentProviderInput {
    pub code: String,
    pub name: String,
    pub secret_key: Option<String>,
    pub success_redirect_url: Option<String>,
    #[graphql(name = "supports3ds")]
    pub supports_3ds: Option<bool>,
    pub require_terms_of_service_consent: Option<bool>,
    /// Stripe webhook endpoint signing secret (`whsec_…`) for inbound
    /// payment events on `/webhooks/stripe/:organization_id`.
    pub webhook_secret: Option<String>,
}

#[derive(InputObject)]
pub struct AdyenPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub api_key: Option<String>,
    pub merchant_account: Option<String>,
    pub hmac_key: Option<String>,
    pub live_prefix: Option<String>,
    pub success_redirect_url: Option<String>,
}

#[derive(InputObject)]
pub struct GocardlessPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub access_code: Option<String>,
    pub success_redirect_url: Option<String>,
    /// GoCardless webhook endpoint secret for inbound payment events.
    pub webhook_secret: Option<String>,
}

/// Creem (merchant of record): invoices are collected through a hosted
/// checkout of a one-time product priced per invoice (`custom_price`).
#[derive(InputObject)]
pub struct CreemPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub api_key: Option<String>,
    /// The one-time Creem product invoices are sold as.
    pub product_id: String,
    /// The product's currency (invoices in other currencies are refused).
    pub currency: Option<String>,
    /// `live` (default) or `test`.
    pub mode: Option<String>,
    /// Creem webhook signing secret for `/webhooks/creem/:organization_id`.
    pub webhook_secret: Option<String>,
    pub success_redirect_url: Option<String>,
}

/// Dodo Payments (merchant of record): hosted checkout of a pay-what-you-want
/// product priced per invoice.
#[derive(InputObject)]
pub struct DodoPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub api_key: Option<String>,
    /// The pay-what-you-want Dodo product invoices are sold as.
    pub product_id: String,
    /// `live` (default) or `test`.
    pub mode: Option<String>,
    /// Dodo webhook signing secret (`whsec_...`).
    pub webhook_secret: Option<String>,
    pub success_redirect_url: Option<String>,
}

/// Iyzico (Turkish card acquirer): hosted checkout form.
#[derive(InputObject)]
pub struct IyzicoPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub api_key: String,
    /// Merchant secret key; signs API calls and merchant notifications.
    pub secret_key: Option<String>,
    /// `live` (default) or `sandbox`.
    pub mode: Option<String>,
    /// Billing city / street Iyzico requires on every form when the customer
    /// has no address of their own.
    pub default_city: String,
    pub default_address: String,
    pub default_country: Option<String>,
    pub success_redirect_url: Option<String>,
}

#[derive(InputObject)]
pub struct CashfreePaymentProviderInput {
    pub code: String,
    pub name: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub success_redirect_url: Option<String>,
}

#[derive(InputObject)]
pub struct FlutterwavePaymentProviderInput {
    pub code: String,
    pub name: String,
    pub secret_key: Option<String>,
    pub success_redirect_url: Option<String>,
}

#[derive(InputObject)]
pub struct MoneyhashPaymentProviderInput {
    pub code: String,
    pub name: String,
    pub api_key: Option<String>,
    pub flow_id: Option<String>,
    pub success_redirect_url: Option<String>,
}

// ---- payment provider customers -------------------------------------------

#[derive(InputObject)]
pub struct CreatePaymentProviderCustomerInput {
    pub customer_id: ID,
    pub payment_provider: String,
    pub payment_provider_code: Option<String>,
    pub provider_customer_id: Option<String>,
}

#[derive(InputObject)]
pub struct UpdatePaymentProviderCustomerInput {
    pub id: ID,
    pub provider_customer_id: Option<String>,
    pub payment_provider_code: Option<String>,
}

// ---- payments + payment requests ------------------------------------------

#[derive(InputObject)]
pub struct CreatePaymentInput {
    pub invoice_id: ID,
    pub amount_cents: i64,
    pub reference: String,
    /// RFC3339 timestamp (optional; defaults to now).
    pub created_at: Option<String>,
}

#[derive(InputObject)]
pub struct CreatePaymentRequestInput {
    pub external_customer_id: String,
    pub email: Option<String>,
    pub amount_cents: Option<i64>,
    pub amount_currency: Option<String>,
}

// ---- integrations (per-provider create) -----------------------------------

#[derive(InputObject)]
pub struct CreateNetsuiteIntegrationInput {
    pub code: String,
    pub name: String,
    pub account_id: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub connection_id: Option<String>,
    pub script_endpoint_url: Option<String>,
    pub token_id: Option<String>,
    pub token_secret: Option<String>,
    pub sync_invoices: Option<bool>,
    pub sync_credit_notes: Option<bool>,
    pub sync_payments: Option<bool>,
}

#[derive(InputObject)]
pub struct CreateXeroIntegrationInput {
    pub code: String,
    pub name: String,
    pub connection_id: Option<String>,
    pub sync_invoices: Option<bool>,
    pub sync_credit_notes: Option<bool>,
    pub sync_payments: Option<bool>,
}

#[derive(InputObject)]
pub struct CreateAnrokIntegrationInput {
    pub code: String,
    pub name: String,
    pub api_key: Option<String>,
    pub connection_id: Option<String>,
}

#[derive(InputObject)]
pub struct CreateAvalaraIntegrationInput {
    pub code: String,
    pub name: String,
    pub account_id: Option<String>,
    pub license_key: Option<String>,
    pub company_code: Option<String>,
    pub connection_id: Option<String>,
}

#[derive(InputObject)]
pub struct CreateHubspotIntegrationInput {
    pub code: String,
    pub name: String,
    pub connection_id: Option<String>,
    pub default_targeted_object: Option<String>,
    pub sync_invoices: Option<bool>,
    pub sync_subscriptions: Option<bool>,
}

#[derive(InputObject)]
pub struct CreateSalesforceIntegrationInput {
    pub code: String,
    pub name: String,
    pub instance_id: Option<String>,
    pub connection_id: Option<String>,
}

#[derive(InputObject)]
pub struct CreateOktaIntegrationInput {
    pub code: Option<String>,
    pub name: Option<String>,
    pub domain: String,
    pub organization_name: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIntegrationInput {
    pub id: ID,
    pub code: Option<String>,
    pub name: Option<String>,
}

// ---- integration collection mappings + mappings ---------------------------

#[derive(InputObject)]
pub struct CreateIntegrationCollectionMappingInput {
    pub integration_id: ID,
    pub mapping_type: String,
    pub external_id: Option<String>,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIntegrationCollectionMappingInput {
    pub id: ID,
    pub external_id: Option<String>,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

#[derive(InputObject)]
pub struct CreateIntegrationMappingInput {
    pub integration_id: ID,
    pub mappable_type: String,
    pub mappable_id: ID,
    pub external_id: String,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIntegrationMappingInput {
    pub id: ID,
    pub external_id: Option<String>,
    pub external_account_code: Option<String>,
    pub external_name: Option<String>,
}

// ---- integration customers ------------------------------------------------

#[derive(InputObject)]
pub struct CreateIntegrationCustomerInput {
    pub customer_id: ID,
    pub integration_id: ID,
    pub external_customer_id: Option<String>,
    pub code: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateIntegrationCustomerInput {
    pub id: ID,
    pub external_customer_id: Option<String>,
    pub code: Option<String>,
}

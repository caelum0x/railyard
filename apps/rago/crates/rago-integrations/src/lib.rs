//! # rago-integrations
//!
//! Money-movement adapters for Rago: **payment providers** ([`payments`],
//! Stripe fully implemented) and **outbound webhooks** ([`webhooks`]). Both go
//! through one injectable [`transport::HttpTransport`], so their real request
//! shape is verified against an in-memory transport in tests; the live
//! reqwest-backed transport is behind the `live` feature (owner-gated).

pub mod integrations;
pub mod oauth;
pub mod payments;
pub mod transport;
pub mod webhooks;

pub use integrations::anrok::AnrokTaxProvider;
pub use integrations::avalara::AvalaraTaxProvider;
pub use integrations::hubspot::HubspotProvider;
pub use integrations::netsuite::NetsuiteProvider;
pub use integrations::salesforce::SalesforceProvider;
pub use integrations::xero::XeroProvider;
pub use integrations::{
    AccountingInvoice, AccountingProvider, CrmCustomer, CrmProvider, FeeTaxResult,
    IntegrationError, SyncResult, TaxBreakdownItem, TaxFeeItem, TaxProvider, TaxRequest,
};
pub use oauth::{with_retry, OAuthClient, OAuthConfig, OAuthTokens};
pub use payments::adyen::AdyenProvider;
pub use payments::gocardless::GocardlessProvider;
pub use payments::provider_webhook::{
    adyen_event, adyen_verify, gocardless_parse, gocardless_verify, ProviderWebhookEvent,
};
pub use payments::stripe::StripeProvider;
pub use payments::stripe_webhook::{
    parse_event, signature_header, verify_signature, IncomingStatus, StripeEvent, WebhookError,
    DEFAULT_TOLERANCE_SECS,
};
pub use payments::{PayableStatus, PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest};
pub use transport::{HttpResponse, HttpTransport, MockTransport, TransportError};
#[cfg(feature = "live")]
pub use transport::ReqwestTransport;
pub use webhooks::catalog::WebhookKind;
pub use webhooks::{deliver, Webhook, WebhookStatus};

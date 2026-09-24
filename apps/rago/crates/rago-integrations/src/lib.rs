//! # rago-integrations
//!
//! Money-movement adapters for Rago: **payment providers** ([`payments`]:
//! Stripe, GoCardless, Adyen, Creem, Dodo Payments, Iyzico) and **outbound webhooks** ([`webhooks`]). Both go
//! through one injectable [`transport::HttpTransport`], so their real request
//! shape is verified against an in-memory transport in tests; the live
//! reqwest-backed transport is behind the `live` feature (owner-gated).

pub mod integrations;
pub mod mint_holds;
pub mod oauth;
pub mod payments;
pub mod transport;
pub mod webhooks;

pub use integrations::anrok::AnrokTaxProvider;
pub use integrations::avalara::AvalaraTaxProvider;
pub use integrations::hubspot::{HubspotObject, HubspotProvider};
pub use integrations::netsuite::{NetsuiteCredentials, NetsuiteProvider};
pub use integrations::salesforce::SalesforceProvider;
pub use integrations::xero::XeroProvider;
pub use integrations::{
    AccountingContact, AccountingInvoice, AccountingLine, AccountingPayment, AccountingProvider,
    CatalogEntry, CrmCustomer, CrmInvoice, CrmProvider, CrmRecordSync, CrmSubscription, FeeTaxResult,
    IntegrationError, SyncResult, TaxBreakdownItem, TaxFeeItem, TaxProvider, TaxRequest,
};
pub use mint_holds::{MintError, MintHold, MintHoldsClient};
pub use oauth::{with_retry, OAuthClient, OAuthConfig, OAuthTokens};
pub use payments::adyen::AdyenProvider;
pub use payments::creem::CreemProvider;
pub use payments::dodo::DodoProvider;
pub use payments::iyzico::{IyzicoBuyer, IyzicoPayment, IyzicoProvider};
pub use payments::gocardless::GocardlessProvider;
pub use payments::provider_webhook::{
    adyen_event, adyen_original_reference, adyen_refund_event, adyen_verify, creem_parse,
    creem_verify, dodo_parse, dodo_verify, gocardless_parse, gocardless_refund_events,
    gocardless_verify, iyzico_verify_notification, CreemWebhook, DodoWebhook, IyzicoNotification,
    ProviderRefundEvent, ProviderWebhookEvent,
};
pub use payments::stripe::StripeProvider;
pub use payments::stripe_webhook::{
    parse_event, signature_header, verify_signature, IncomingStatus, StripeEvent, StripeIntentProgress,
    WebhookError,
    DEFAULT_TOLERANCE_SECS,
};
pub use payments::{
    PayableStatus, PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, RefundOutcome,
    RefundRequest, RefundState,
};
pub use transport::{
    default_transport, DefaultTransport, DisabledTransport, HttpResponse, HttpTransport,
    MockTransport, TransportError, LIVE,
};
#[cfg(feature = "live")]
pub use transport::ReqwestTransport;
pub use webhooks::catalog::WebhookKind;
pub use webhooks::{deliver, Webhook, WebhookStatus};

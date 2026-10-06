//! # rago-db
//!
//! Postgres persistence for Rago's billing engine, on top of `sqlx`. Provides:
//!
//! * embedded [`MIGRATOR`] migrations for the core billing schema (a faithful
//!   subset of Lago's `db/structure.sql`),
//! * [`repo::Db`] — a repository with the finders the billing services need
//!   (charges, metrics, subscriptions, plans, taxes, events) plus invoice/fee
//!   inserts,
//! * [`mapping`] — row → [`rago_core::models`] conversions.
//!
//! Queries are runtime (`sqlx::query`) so the crate builds without a live
//! database; the domain math lives in `rago-billing`.

pub mod billing_gaps;
pub mod charge_filters;
pub mod charge_usage;
pub mod collection;
pub mod credit_note_credits;
pub mod customer_lifecycle;
pub mod data_api;
pub mod dunning;
pub mod error;
pub mod graphql_admin;
pub mod graphql_admin2;
pub mod graphql_admin3;
pub mod graphql_admin4;
pub mod graphql_admin5;
pub mod integration_hub;
pub mod lifecycle;
pub mod integration_sweeps;
pub mod mapping;
pub mod numbering;
pub mod order_flow_console;
pub mod orders;
pub mod pay_in_advance;
pub mod payment_actions;
pub mod portal;
pub mod progressive;
pub mod provider_checkouts;
pub mod repo;
pub mod termination;
pub mod receipts;
pub mod refunds;
pub mod rest_extras;
pub mod secrets;
pub mod sso_state;
pub mod tenancy;
pub mod wallet_credits;
pub mod wallet_realtime;
pub mod webhook_deliveries;

pub use dunning::{
    DunningCampaignPolicy, DunningCandidate, DunningOverviewRow, DunningThresholdRow,
    OverdueInvoiceRow,
};
pub use billing_gaps::{CustomerAddress, HoldListRow, HoldTotals, ReversibleCapture};
pub use collection::{PaymentAttemptState, ProviderCredentials, StoreSecretError};
pub use credit_note_credits::{AppliedCreditNotes, InvoiceCreditNoteCredit};
pub use charge_usage::LoadedCharge;
pub use portal::PortalSession;
pub use provider_checkouts::CheckoutPayment;
pub use wallet_realtime::{RealtimeHold, RealtimeWallet};
pub use error::{DbError, Result};
pub use graphql_admin::{AddOnRow, DunningCampaignRow, WebhookRow};
pub use graphql_admin2::{
    AlertRow, AlertThresholdRow, BillingEntityRow, FeatureRow, FixedChargeRow,
    InvoiceCustomSectionRow, PaymentRow,
};
pub use graphql_admin3::{
    IntegrationCollectionMappingRow, IntegrationCustomerRow, IntegrationItemRow,
    IntegrationMappingRow, IntegrationRow, PaymentMethodRow, PaymentProviderCustomerRow,
    PaymentProviderRow, PaymentRequestRow,
};
pub use graphql_admin4::{
    CommitmentRow, InviteRow, MembershipRow, PlanAppliedRateCardRow, PricingUnitRow,
    ProductCategoryRow, ProductFilterRow, ProductRow, RateCardRateRow, RateCardRow, RatePhaseRow,
    RoleRow, UserRow,
};
pub use graphql_admin5::{
    AiConversationRow, AiMessageRow, OrderFormRow, OrderRow, QuoteRow, QuoteVersionRow,
};
pub use refunds::{NewRefund, RefundRow, RefundablePayment};
pub use repo::{
    AdvisoryLock, AnalyticsPoint, BilledInvoice, Db, InvoiceCollectionPoint, LeasedJob,
    PrepaidSettlement, RetryingWebhook,
};
pub use webhook_deliveries::{NewWebhookDelivery, WebhookDeliveryRow};

/// Embedded migrations (see `migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

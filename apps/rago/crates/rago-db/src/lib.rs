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

pub mod error;
pub mod graphql_admin;
pub mod graphql_admin2;
pub mod graphql_admin3;
pub mod graphql_admin4;
pub mod graphql_admin5;
pub mod mapping;
pub mod repo;

pub use error::{DbError, Result};
pub use graphql_admin::{AddOnRow, DunningCampaignRow, WebhookRow};
pub use graphql_admin5::{
    AiConversationRow, AiMessageRow, OrderFormRow, OrderRow, QuoteRow, QuoteVersionRow,
};
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
pub use repo::{
    AnalyticsPoint, BilledInvoice, Db, InvoiceCollectionPoint, LeasedJob, RetryingWebhook,
};

/// Embedded migrations (see `migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

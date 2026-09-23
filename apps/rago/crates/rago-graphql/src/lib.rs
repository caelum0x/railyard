//! # rago-graphql
//!
//! The GraphQL dashboard API for Rago (a faithful port of `app/graphql`),
//! built on `async-graphql`. It reuses the ported `rago-db` repositories and
//! `rago-billing` engine — the resolvers are wiring, not new business logic.
//!
//! The roots are `MergedObject`s combining two halves:
//! * [`Query`] = [`query::Query`] (core reads: customers, plans, metrics,
//!   coupons, subscriptions, invoices, wallets, analytics, connections) +
//!   [`admin_queries::AdminQuery`] (list/show for taxes, add-ons, single
//!   plan/metric/subscription/wallet/coupon/credit-note/dunning/webhook-endpoint
//!   + charge filters).
//! * [`Mutation`] = [`mutation::Mutation`] (core create/terminate) +
//!   [`admin_mutations::AdminMutation`] (the dashboard CRUD slice: update/
//!   destroy for customers, plans, charges + charge filters, billable metrics,
//!   add-ons, coupons + applied coupons, subscriptions, taxes, dunning,
//!   webhook endpoints; invoice finalize/refresh/update/lose-dispute/download;
//!   credit-note create/update/void/download; wallet update; webhook retry).
//! * Auth: every resolver is gated behind a real [`Principal`] resolved from
//!   the request's bearer token; see [`context`].

pub mod admin_inputs;
pub mod admin_inputs2;
pub mod admin_inputs3;
pub mod admin_inputs4;
pub mod admin_inputs5;
pub mod admin_mutations;
pub mod admin_mutations2;
pub mod admin_mutations3;
pub mod admin_mutations4;
pub mod admin_mutations5;
pub mod admin_queries;
pub mod admin_queries2;
pub mod admin_queries3;
pub mod admin_queries4;
pub mod admin_queries5;
pub mod auth;
pub mod context;
pub mod mutation;
pub mod query;
pub mod types;
pub mod webhook_emit;
pub mod types_admin;
pub mod types_admin2;
pub mod types_admin3;
pub mod types_admin4;
pub mod types_admin5;

use async_graphql::{EmptySubscription, MergedObject, Schema};
use rago_db::Db;

pub use context::{current_user, GraphQlData, Principal, UserSession};

/// The Rago GraphQL mutation root: core resource mutations
/// ([`mutation::Mutation`]) merged with the dashboard admin CRUD mutations —
/// slice 1 [`admin_mutations::AdminMutation`], slice 2
/// [`admin_mutations2::AdminMutation2`], slice 3 (payment providers +
/// integrations) [`admin_mutations3::AdminMutation3`], slice 4 (catalog v2 +
/// auth/RBAC) [`admin_mutations4::AdminMutation4`], slice 5 (orders/quotes + AI
/// + data exports + SSO shape) [`admin_mutations5::AdminMutation5`].
#[derive(MergedObject, Default)]
pub struct Mutation(
    mutation::Mutation,
    admin_mutations::AdminMutation,
    admin_mutations2::AdminMutation2,
    admin_mutations3::AdminMutation3,
    admin_mutations4::AdminMutation4,
    admin_mutations5::AdminMutation5,
);

/// The Rago GraphQL query root: core reads ([`query::Query`]) merged with the
/// admin list/show queries (slices 1–5).
#[derive(MergedObject, Default)]
pub struct Query(
    query::Query,
    admin_queries::AdminQuery,
    admin_queries2::AdminQuery2,
    admin_queries3::AdminQuery3,
    admin_queries4::AdminQuery4,
    admin_queries5::AdminQuery5,
);

/// The Rago GraphQL schema type.
pub type RagoSchema = Schema<Query, Mutation, EmptySubscription>;

/// Build the schema, wiring the database as schema-level data.
pub fn build_schema(db: Db) -> RagoSchema {
    Schema::build(Query::default(), Mutation::default(), EmptySubscription)
        .data(GraphQlData { db })
        .finish()
}

/// The email transport resolvers use to send customer emails (resend).
#[derive(Clone)]
pub struct GraphMailer(pub std::sync::Arc<dyn rago_mail::EmailTransport>);

/// Build the schema with the API's email transport, so mutations that send
/// email (`resendInvoiceEmail`) use the same mailer as the REST surface.
pub fn build_schema_with_mailer(db: Db, mailer: std::sync::Arc<dyn rago_mail::EmailTransport>) -> RagoSchema {
    Schema::build(Query::default(), Mutation::default(), EmptySubscription)
        .data(GraphQlData { db })
        .data(GraphMailer(mailer))
        .finish()
}

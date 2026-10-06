//! # rago-api
//!
//! REST surface for Rago — a faithful port of Lago's `api/v1` controllers on
//! axum, wired to [`rago_db`] repositories and the [`rago_billing`] engine.
//!
//! * Bearer API-key auth ([`auth`]) gates every `/api/v1` route against the
//!   `api_keys` table — no bypass.
//! * Handlers ([`handlers`]) implement customers, billable metrics, plans (+
//!   charges), subscriptions (create/terminate), events (validate+create),
//!   subscription billing → invoice, invoices, wallets + wallet transactions
//!   (top-up), coupons, and credit notes.
//! * Responses use Lago's root-wrapped envelopes and `V1::*Serializer` shapes
//!   ([`serializers`]); errors use the `Api::ApiErrors` envelope ([`error`]).

pub mod analytics;
pub mod auth;
pub mod breadth;
pub mod catalog_v2;
pub mod charge_filters;
pub mod checkout_webhooks;
pub mod client_ip;
pub mod crud;
pub mod customer_lists;
pub mod emit;
pub mod error;
pub mod lago_plans;
pub mod lago_wallets;
pub mod fixed_charges;
pub mod graphql;
pub mod handlers;
pub mod integration_hub;
pub mod integrations;
pub mod observability;
pub mod order_flow;
pub mod pagination;
pub mod payment_actions;
pub mod payments;
pub mod reads;
pub mod refunds;
pub mod phase14;
pub mod phase15;
pub mod plan_change;
pub mod portal;
pub mod resources;
pub mod sdk_parity;
pub mod serializers;
pub mod state;
pub mod subs;
pub mod tenancy;
pub mod thresholds;
pub mod usage;
pub mod wallet_console;
pub mod webhooks;

use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use rago_core::enums::{AggregationType, ChargeModel};
use rago_core::models::{BillableMetric, Event};
use rago_core::money::Currency;
use rago_billing::aggregation;
use rago_billing::charge_models::{self, PricingStructure};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

pub use state::AppState;

/// Build the application router with database-backed state.
pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/events", post(handlers::create_event).get(breadth::list_events))
        .route("/events/:transaction_id", get(sdk_parity::show_event))
        .route("/fees", get(sdk_parity::list_fees))
        .route("/fees/:id", get(sdk_parity::show_fee).put(sdk_parity::update_fee).delete(sdk_parity::destroy_fee))
        .route("/customers/:external_id/applied_coupons/:id", axum::routing::delete(sdk_parity::destroy_applied_coupon))
        .route("/invoices/preview", post(sdk_parity::preview_invoice))
        .route("/dunning_campaigns/:code", get(sdk_parity::show_dunning_campaign).put(sdk_parity::update_dunning_campaign).delete(sdk_parity::destroy_dunning_campaign))
        .route("/wallets/:id/recurring_rules", post(breadth::create_wallet_recurring_rule).get(sdk_parity::list_wallet_recurring_rules))
        .route("/webhook_deliveries/:id/retry", post(sdk_parity::retry_webhook_delivery))
        .route("/customers", post(handlers::create_customer).get(handlers::list_customers))
        .route("/customers/:external_id", get(handlers::show_customer).delete(crud::destroy_customer))
        .route("/customers/:external_id/subscriptions", get(crud::customer_subscriptions))
        .route("/customers/:external_id/invoices", get(crud::customer_invoices))
        .route("/customers/:external_id/portal_url", get(crud::customer_portal_url))
        .route("/customers/:external_id/portal_sessions", post(portal::create_session).delete(portal::revoke_sessions))
        .route("/customers/:external_id/checkout_url", post(crud::customer_checkout_url))
        .route("/customers/:external_id/applied_coupons", get(customer_lists::customer_applied_coupons))
        .route("/customers/:external_id/credit_notes", get(customer_lists::customer_credit_notes))
        .route("/customers/:external_id/payments", get(customer_lists::customer_payments))
        .route("/billable_metrics", post(handlers::create_billable_metric).get(crud::list_billable_metrics))
        .route("/billable_metrics/evaluate_expression", post(crud::evaluate_expression))
        .route("/billable_metrics/:code", get(crud::show_billable_metric).put(crud::update_billable_metric).delete(crud::destroy_billable_metric))
        .route("/plans", post(handlers::create_plan).get(crud::list_plans))
        .route("/plans/:code", get(crud::show_plan).put(crud::update_plan).delete(crud::destroy_plan))
        .route(
            "/plans/:code/entitlements",
            get(lago_plans::plan_entitlements).post(lago_plans::create_plan_entitlements).patch(lago_plans::patch_plan_entitlements),
        )
        .route("/plans/:code/entitlements/:feature", get(lago_plans::plan_entitlement).delete(lago_plans::destroy_plan_entitlement))
        .route("/plans/:code/entitlements/:feature/privileges/:privilege", axum::routing::delete(lago_plans::destroy_plan_entitlement_privilege))
        .route("/plans/:code/charges/:id/filters", get(lago_plans::plan_charge_filters).post(lago_plans::create_plan_charge_filter))
        .route(
            "/plans/:code/charges/:id/filters/:filter_id",
            get(lago_plans::show_plan_charge_filter).put(lago_plans::update_plan_charge_filter).delete(lago_plans::destroy_plan_charge_filter),
        )
        .route("/plans/:code/charges", post(crud::create_plan_charge).get(crud::list_plan_charges))
        .route("/plans/:code/fixed_charges", get(fixed_charges::index).post(fixed_charges::create))
        .route(
            "/plans/:code/fixed_charges/:fixed_charge_code",
            get(fixed_charges::show).put(fixed_charges::update).delete(fixed_charges::destroy),
        )
        .route("/plans/:code/usage_thresholds", get(thresholds::list_plan_thresholds).put(thresholds::replace_plan_thresholds))
        .route("/plans/:code/charges/:id", get(crud::show_plan_charge).put(crud::update_plan_charge).delete(crud::destroy_plan_charge))
        .route("/entitlements", post(handlers::create_entitlement))
        .route("/subscriptions", post(handlers::create_subscription).get(breadth::list_subscriptions))
        .route("/subscriptions/:external_id", get(subs::show_subscription).put(subs::update_subscription).delete(handlers::terminate_subscription))
        .route("/subscriptions/:external_id/invoices", post(handlers::bill_subscription))
        .route("/subscriptions/:external_id/plan_change", get(plan_change::show).post(plan_change::create).delete(plan_change::destroy))
        .route("/subscriptions/:external_id/lifetime_usage", get(subs::subscription_lifetime_usage).put(subs::update_subscription_lifetime_usage))
        .route("/subscriptions/:external_id/usage_thresholds", get(thresholds::list_subscription_thresholds).put(thresholds::replace_subscription_thresholds))
        .route("/subscriptions/:external_id/usage_thresholds/check", post(thresholds::check_subscription_thresholds))
        .route(
            "/subscriptions/:external_id/entitlements",
            post(handlers::create_subscription_entitlement).get(lago_plans::subscription_entitlements).patch(lago_plans::patch_subscription_entitlements),
        )
        .route("/subscriptions/:external_id/entitlements/:feature", axum::routing::delete(lago_plans::destroy_subscription_entitlement))
        .route(
            "/subscriptions/:external_id/entitlements/:feature/privileges/:privilege",
            axum::routing::delete(lago_plans::destroy_subscription_entitlement_privilege),
        )
        .route("/subscriptions/:external_id/charges", get(lago_plans::subscription_charges))
        .route("/subscriptions/:external_id/charges/:code", get(lago_plans::subscription_charge).put(lago_plans::update_subscription_charge))
        .route(
            "/subscriptions/:external_id/charges/:code/filters",
            get(lago_plans::subscription_charge_filters).post(lago_plans::create_subscription_charge_filter),
        )
        .route(
            "/subscriptions/:external_id/charges/:code/filters/:id",
            get(lago_plans::show_subscription_charge_filter)
                .put(lago_plans::update_subscription_charge_filter)
                .delete(lago_plans::destroy_subscription_charge_filter),
        )
        .route("/subscriptions/:external_id/fixed_charges", get(lago_plans::subscription_fixed_charges))
        .route(
            "/subscriptions/:external_id/fixed_charges/:code",
            get(lago_plans::subscription_fixed_charge).put(lago_plans::update_subscription_fixed_charge),
        )
        .route("/features/:code/privileges/:privilege", axum::routing::delete(lago_plans::destroy_feature_privilege))
        .route("/subscriptions/:external_id/alerts", post(subs::create_alert).get(subs::list_alerts).delete(subs::destroy_all_alerts))
        .route("/subscriptions/:external_id/alerts/:code", get(subs::show_alert).put(subs::update_alert).delete(subs::destroy_alert))
        .route("/subscriptions/:external_id/usage_alerts", post(phase14::create_usage_alert).get(phase14::list_usage_alerts))
        .route("/customers/:external_customer_id/current_usage", get(usage::current_usage))
        .route("/customers/:external_customer_id/projected_usage", get(usage::projected_usage))
        .route("/customers/:external_customer_id/past_usage", get(usage::past_usage))
        .route("/customers/:external_customer_id/render_allowance", post(usage::render_allowance))
        .route("/reels/usage", post(usage::record_reel_usage))
        .route("/reels/catalog", get(usage::reel_catalog))
        .route("/invoices", get(breadth::list_invoices).post(breadth::create_one_off_invoice))
        .route("/add_ons", post(breadth::create_add_on).get(crud::list_add_ons))
        .route("/add_ons/:code", get(crud::show_add_on).put(crud::update_add_on).delete(crud::destroy_add_on))
        .route("/quotes", post(breadth::create_quote).get(order_flow::list_quotes))
        .route("/quotes/:id", get(order_flow::show_quote))
        .route("/quotes/:id/accept", post(breadth::accept_quote))
        .route("/quotes/:id/void", post(order_flow::void_quote))
        .route("/quotes/:id/versions", post(order_flow::create_version))
        .route("/quotes/:id/versions/:version_id", put(order_flow::update_version))
        .route("/quotes/:id/versions/:version_id/approve", post(order_flow::approve_version))
        .route("/quotes/:id/versions/:version_id/void", post(order_flow::void_version))
        .route("/quotes/:id/order_forms", post(order_flow::create_order_form))
        .route("/order_forms", get(order_flow::list_order_forms))
        .route("/order_forms/:id", get(order_flow::show_order_form))
        .route("/order_forms/:id/sign", post(order_flow::sign_order_form))
        .route("/order_forms/:id/void", post(order_flow::void_order_form))
        .route("/orders", get(order_flow::list_orders))
        .route("/orders/:id", get(order_flow::show_order))
        .route("/orders/:id/execute", post(order_flow::execute_order))
        .route("/orders/:id/void", post(order_flow::void_order))
        .route("/data_exports", post(breadth::create_data_export))
        .route("/data_exports/:id", get(breadth::show_data_export))
        .route("/data_exports/:id/download", get(breadth::download_data_export))
        .route("/emails", get(breadth::list_captured_emails))
        .route("/invoices/:id", get(handlers::show_invoice).put(sdk_parity::update_invoice).delete(payment_actions::delete_draft_invoice))
        .route("/invoices/:id/download", get(handlers::download_invoice_pdf))
        .route("/invoices/:id/download.xml", get(handlers::download_invoice_xml))
        .route("/invoices/:id/apply_provider_taxes", post(integrations::apply_provider_taxes_to_invoice))
        .route("/invoices/:id/refund", post(refunds::refund_invoice))
        .route("/invoices/:id/refunds", get(refunds::list_invoice_refunds))
        .route("/invoices/:id/void", post(subs::void_invoice))
        .route("/invoices/:id/finalize", put(subs::finalize_invoice))
        .route("/invoices/:id/refresh", put(subs::refresh_invoice))
        .route("/invoices/:id/lose_dispute", post(subs::lose_dispute))
        .route("/invoices/:id/resend_email", post(subs::resend_invoice_email))
        .route("/invoices/:id/sync_salesforce_id", put(subs::sync_salesforce_id))
        .route("/invoices/:id/retry", post(subs::retry_invoice))
        .route("/invoices/:id/retry_payment", post(subs::retry_payment))
        .route("/invoices/:id/payment_url", post(subs::invoice_payment_url))
        .route("/invoices/:id/payment_action", get(payment_actions::show_payment_action))
        .route("/invoices/:id/payment_action/notify", post(payment_actions::notify_payment_action))
        .route("/activity_logs", get(handlers::list_activity_logs))
        .route("/credit_notes", post(handlers::create_credit_note).get(subs::list_credit_notes))
        .route("/wallets", post(handlers::create_wallet).get(sdk_parity::list_wallets))
        .route("/wallets/:id", get(handlers::show_wallet).put(crud::update_wallet).delete(crud::terminate_wallet))
        .route("/wallets/:id/realtime", get(crud::show_wallet_realtime).put(crud::update_wallet_realtime))
        .route("/wallets/:id/realtime/overview", get(wallet_console::overview))
        .route("/wallets/:id/realtime/holds", get(wallet_console::holds))
        .route("/wallets/:id/realtime/top_ups", post(wallet_console::create_top_up))
        .route("/wallets/:id/realtime/reversals", post(wallet_console::retry_reversals))
        .route("/wallets/:id/transactions", get(reads::wallet_transactions))
        .route("/wallet_transactions", post(handlers::create_wallet_transaction))
        .route("/coupons", post(handlers::create_coupon).get(crud::list_coupons))
        .route("/coupons/:code", get(crud::show_coupon).put(crud::update_coupon).delete(crud::destroy_coupon))
        .route("/applied_coupons", post(handlers::create_applied_coupon).get(customer_lists::list_applied_coupons))
        .route("/credit_notes/:id", get(handlers::show_credit_note).put(subs::update_credit_note))
        .route("/credit_notes/:id/download", get(phase14::download_credit_note_pdf))
        .route("/credit_notes/:id/download_xml", post(subs::download_credit_note_xml))
        .route("/credit_notes/:id/void", put(subs::void_credit_note))
        .route("/credit_notes/:id/refund", post(refunds::refund_credit_note))
        .route("/credit_notes/:id/resend_email", post(subs::resend_credit_note_email))
        .route("/credit_notes/estimate", post(handlers::estimate_credit_note))
        .route("/invoices/:id/receipt", get(phase14::download_payment_receipt_pdf))
        .route("/invoices/:id/sync_accounting", post(phase14::sync_invoice_accounting))
        .route("/customers/:external_id/sync_crm", post(phase14::sync_customer_crm))
        .route("/invoices/:id/sync_crm", post(phase14::sync_invoice_crm))
        .route("/subscriptions/:external_id/sync_crm", post(phase14::sync_subscription_crm))
        .route("/payments/:id/sync_accounting", post(phase14::sync_payment_accounting))
        .route("/credit_notes/:id/sync_accounting", post(integration_hub::sync_credit_note_accounting))
        .route("/integrations", post(integration_hub::create).get(integration_hub::list))
        .route("/integrations/:id", get(integration_hub::show).put(integration_hub::update).delete(integration_hub::destroy))
        .route("/integrations/:id/refresh", post(phase14::refresh_integration))
        .route("/integrations/:id/oauth/authorize", get(integration_hub::oauth_authorize))
        .route("/integrations/:id/catalog", post(integration_hub::refresh_catalog))
        .route("/integrations/:id/mappings", post(integration_hub::create_mapping))
        .route("/integrations/:id/mappings/:mapping_id", axum::routing::delete(integration_hub::destroy_mapping))
        .route("/integrations/:id/collection_mappings", post(integration_hub::upsert_collection_mapping))
        .route("/integrations/:id/collection_mappings/:mapping_id", axum::routing::delete(integration_hub::destroy_collection_mapping))
        .route("/integrations/:id/customers", post(integration_hub::link_customer))
        .route("/integrations/:id/customers/:link_id", axum::routing::delete(integration_hub::unlink_customer))
        .route("/integration_sync_status", get(integration_hub::sync_status))
        .route("/customers/:external_customer_id/lifetime_usage", get(phase15::lifetime_usage))
        .route("/dunning_campaigns", post(phase15::create_dunning_campaign).get(phase15::list_dunning_campaigns))
        .route("/fees/estimate", post(estimate_fee))
        // --- new resources: taxes, webhook endpoints, billing entities, orgs, features
        .route("/taxes", post(resources::create_tax).get(resources::list_taxes))
        .route("/taxes/:code", get(resources::show_tax).put(resources::update_tax).delete(resources::destroy_tax))
        .route("/webhook_endpoints", post(resources::create_webhook_endpoint).get(resources::list_webhook_endpoints))
        .route("/webhook_endpoints/:id", get(resources::show_webhook_endpoint).put(resources::update_webhook_endpoint).delete(resources::destroy_webhook_endpoint))
        // --- webhook deliveries feed (read surface)
        .route("/webhook_deliveries", get(reads::list_webhook_deliveries))
        .route("/webhook_deliveries/:id", get(reads::show_webhook_delivery))
        // --- dunning overview (overdue invoices + campaign/attempt status)
        .route("/dunning", get(reads::list_dunning))
        .route("/billing_entities", post(resources::create_billing_entity).get(resources::list_billing_entities))
        .route("/billing_entities/:code", get(resources::show_billing_entity).put(resources::update_billing_entity))
        .route("/organizations", get(resources::show_organization).put(resources::update_organization))
        .route("/features", post(resources::create_feature).get(resources::list_features))
        .route("/features/:code", get(resources::show_feature).put(resources::update_feature).delete(resources::destroy_feature))
        // --- payments, payment requests, payment receipts
        .route("/payments", post(payments::create_payment).get(payments::list_payments))
        .route("/payments/:id", get(payments::show_payment))
        .route("/payment_requests", post(payments::create_payment_request).get(payments::list_payment_requests))
        .route("/payment_requests/:id", get(payments::show_payment_request))
        .route("/payment_receipts", get(payments::list_payment_receipts))
        .route("/payment_receipts/:id", get(payments::show_payment_receipt))
        .route("/customers/:external_id/payment_requests", get(payments::customer_payment_requests))
        // --- Lago REST parity (round 2): wallets, metadata, payment methods, wallet transactions
        .route("/customers/:external_id/wallets", post(lago_wallets::create_customer_wallet).get(lago_wallets::list_customer_wallets))
        .route(
            "/customers/:external_id/wallets/:code",
            get(lago_wallets::show_customer_wallet).put(lago_wallets::update_customer_wallet).delete(lago_wallets::terminate_customer_wallet),
        )
        .route(
            "/customers/:external_id/wallets/:code/alerts",
            post(lago_wallets::create_wallet_alert).get(lago_wallets::list_wallet_alerts).delete(lago_wallets::destroy_all_wallet_alerts),
        )
        .route(
            "/customers/:external_id/wallets/:code/alerts/:alert_code",
            get(lago_wallets::show_wallet_alert).put(lago_wallets::update_wallet_alert).delete(lago_wallets::destroy_wallet_alert),
        )
        .route(
            "/customers/:external_id/wallets/:code/metadata",
            post(lago_wallets::create_customer_wallet_metadata)
                .put(lago_wallets::update_customer_wallet_metadata)
                .delete(lago_wallets::destroy_customer_wallet_metadata),
        )
        .route("/customers/:external_id/wallets/:code/metadata/:key", axum::routing::delete(lago_wallets::destroy_customer_wallet_metadata_key))
        .route("/customers/:external_id/payment_methods/:id", axum::routing::delete(lago_wallets::destroy_payment_method))
        .route("/customers/:external_id/payment_methods/:id/set_as_default", put(lago_wallets::set_default_payment_method))
        .route(
            "/wallets/:id/metadata",
            post(lago_wallets::create_wallet_metadata).put(lago_wallets::update_wallet_metadata).delete(lago_wallets::destroy_wallet_metadata),
        )
        .route("/wallets/:id/metadata/:key", axum::routing::delete(lago_wallets::destroy_wallet_metadata_key))
        .route(
            "/plans/:code/metadata",
            post(lago_wallets::create_plan_metadata).put(lago_wallets::update_plan_metadata).delete(lago_wallets::destroy_plan_metadata),
        )
        .route("/plans/:code/metadata/:key", axum::routing::delete(lago_wallets::destroy_plan_metadata_key))
        .route(
            "/credit_notes/:id/metadata",
            post(lago_wallets::create_credit_note_metadata)
                .put(lago_wallets::update_credit_note_metadata)
                .delete(lago_wallets::destroy_credit_note_metadata),
        )
        .route("/credit_notes/:id/metadata/:key", axum::routing::delete(lago_wallets::destroy_credit_note_metadata_key))
        .route("/wallet_transactions/:id", get(lago_wallets::show_wallet_transaction))
        .route("/wallet_transactions/:id/payment_url", post(lago_wallets::wallet_transaction_payment_url))
        .route("/wallet_transactions/:id/consumptions", get(lago_wallets::wallet_transaction_consumptions))
        .route("/wallet_transactions/:id/fundings", get(lago_wallets::wallet_transaction_fundings))
        .route("/wallets/:id/wallet_transactions", get(lago_wallets::wallet_wallet_transactions))
        .route("/customers/:external_id/payment_methods", get(payments::customer_payment_methods))
        .route("/payment_receipts/:invoice_id/resend_email", post(payments::resend_payment_receipt))
        // --- v1 analytics (data API)
        .route("/analytics/gross_revenue", get(phase15::v2_gross_revenue))
        .route("/analytics/mrr", get(phase15::v2_mrr))
        .route("/analytics/overdue_balance", get(phase15::v2_overdue_balance))
        .route("/analytics/invoice_collection", get(phase15::v2_invoice_collection))
        .route("/analytics/invoiced_usage", get(analytics::v1_invoiced_usage));

    let v2 = Router::new()
        .route("/events/batch", post(phase15::v2_events_batch))
        .route("/events/estimate_fees", post(estimate_fee))
        .route("/analytics/gross_revenue", get(phase15::v2_gross_revenue))
        .route("/analytics/mrr", get(phase15::v2_mrr))
        .route("/analytics/overdue_balance", get(phase15::v2_overdue_balance))
        .route("/analytics/invoice_collection", get(phase15::v2_invoice_collection))
        .route("/analytics/invoiced_usage", get(analytics::v1_invoiced_usage))
        // --- v2 catalog: products, categories, rate cards (+rates), plan applied rate cards + phases
        .route("/products", post(catalog_v2::create_product).get(catalog_v2::list_products))
        .route("/products/:id", get(catalog_v2::show_product).put(catalog_v2::update_product).delete(catalog_v2::destroy_product))
        .route("/product_categories", post(catalog_v2::create_product_category).get(catalog_v2::list_product_categories))
        .route("/rate_cards", post(catalog_v2::create_rate_card).get(catalog_v2::list_rate_cards))
        .route("/rate_cards/:id", get(catalog_v2::show_rate_card).put(catalog_v2::update_rate_card).delete(catalog_v2::destroy_rate_card))
        .route("/rate_cards/:id/rates", post(catalog_v2::create_rate_card_rate).get(catalog_v2::list_rate_card_rates))
        .route("/plans/:code/applied_rate_cards", post(catalog_v2::create_applied_rate_card).get(catalog_v2::list_applied_rate_cards))
        .route("/applied_rate_cards/:id/rate_phases", post(catalog_v2::create_rate_phase).get(catalog_v2::list_rate_phases));

    // Tenant isolation: reject any request referencing another org's record ids.
    let v1 = v1.layer(axum::middleware::from_fn_with_state(state.clone(), tenancy::guard));
    let v2 = v2.layer(axum::middleware::from_fn_with_state(state.clone(), tenancy::guard));

    Router::new()
        .route("/health", get(health))
        .route("/admin", get(admin_page))
        .route("/admin/core.js", get(admin_core_js))
        .route("/admin/views.js", get(admin_views_js))
        .route("/admin/views2.js", get(admin_views2_js))
        .route("/admin/billing.js", get(admin_billing_js))
        .route("/admin/integrations.js", get(admin_integrations_js))
        .route("/admin/quotes.js", get(admin_quotes_js))
        .route("/admin/keys.js", get(admin_keys_js))
        .route("/admin/sso/callback", get(admin_page))
        .route("/integrations/oauth/callback", get(integration_hub::oauth_callback))
        .route("/auth/sso/authorize", post(integration_hub::sso_authorize))
        .route("/auth/sso/callback", post(integration_hub::sso_callback))
        .route("/metrics", get(observability::metrics_handler))
        .route(
            "/graphql",
            get(graphql::graphql_playground).post(graphql::graphql_handler),
        )
        .route("/webhooks/stripe/:organization_id", post(webhooks::stripe_webhook))
        .route("/webhooks/gocardless/:organization_id", post(webhooks::gocardless_webhook))
        .route("/webhooks/adyen/:organization_id", post(webhooks::adyen_webhook))
        .route("/portal/:token", get(portal::home))
        .route("/portal/:token/invoices/:invoice_id/download", get(portal::download_invoice))
        .route("/portal/:token/invoices/:invoice_id/pay", post(portal::pay_invoice))
        .route("/portal/:token/wallets/:wallet_id/top_up", post(portal::top_up))
        .route("/portal/:token/subscriptions/:subscription_id/cancel", get(portal::manage_subscription).post(portal::cancel_subscription))
        .route("/portal/:token/subscriptions/:subscription_id/downgrade", post(portal::downgrade_subscription))
        .route("/portal/:token/subscriptions/:subscription_id/keep_plan", post(portal::keep_plan))
        .route(
            "/portal/:token/subscriptions/:subscription_id/cancel_at_period_end",
            post(portal::cancel_at_period_end),
        )
        .route("/portal/:token/subscriptions/:subscription_id/resume", post(portal::resume_subscription))
        .route("/portal/:token/payment_method", post(portal::payment_method))
        .route("/webhooks/creem/:organization_id", post(checkout_webhooks::creem_webhook))
        .route("/webhooks/dodo/:organization_id", post(checkout_webhooks::dodo_webhook))
        .route("/webhooks/iyzico/:organization_id", post(checkout_webhooks::iyzico_webhook))
        .nest("/api/v1", v1)
        .nest("/api/v2", v2)
        // Observability + protection middleware (applied to every route).
        .layer(axum::middleware::from_fn(observability::track_metrics))
        .layer(axum::middleware::from_fn(observability::rate_limit))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "rago-api" }))
}

/// `GET /admin` — the operator console (static HTML + vanilla JS, no build
/// step): overview analytics, customers, subscriptions, plans and metrics,
/// invoices with their actions, credit notes, wallets, coupons, add-ons, taxes,
/// webhooks, dunning, events, exports, settings and API keys. The page and scripts are
/// served unauthenticated, but every data request carries the operator's
/// `Authorization: Bearer <api-key>` against the same authenticated API — there
/// is no server-side data bypass.
async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../assets/admin.html"))
}

/// A static console script (embedded at build time; no build step).
fn admin_js(body: &'static str) -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}
async fn admin_core_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-core.js"))
}
async fn admin_views_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-views.js"))
}
async fn admin_views2_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-views2.js"))
}
async fn admin_billing_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-billing.js"))
}
async fn admin_integrations_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-integrations.js"))
}
async fn admin_quotes_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-quotes.js"))
}
async fn admin_keys_js() -> impl axum::response::IntoResponse {
    admin_js(include_str!("../assets/admin-keys.js"))
}

// --- fees/estimate: drives the billing engine without persistence -----------

#[derive(Debug, Deserialize)]
pub struct EstimateRequest {
    pub aggregation_type: i32,
    pub field_name: Option<String>,
    pub charge_model: i32,
    pub properties: Value,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub events: Vec<EstimateEvent>,
}
fn default_currency() -> String {
    "USD".to_string()
}
#[derive(Debug, Deserialize)]
pub struct EstimateEvent {
    pub properties: Value,
}
#[derive(Debug, Serialize)]
pub struct EstimateResponse {
    pub units: String,
    pub amount: String,
    pub unit_amount: String,
    pub amount_details: Value,
    pub events_count: i64,
}

async fn estimate_fee(
    _auth: auth::Authed,
    Json(req): Json<EstimateRequest>,
) -> Result<Json<EstimateResponse>, error::ApiError> {
    let aggregation_type = AggregationType::try_from(req.aggregation_type)
        .map_err(|e| error::ApiError::validation("aggregation_type", &e.to_string()))?;
    let charge_model = ChargeModel::try_from(req.charge_model)
        .map_err(|e| error::ApiError::validation("charge_model", &e.to_string()))?;
    let currency = Currency::new(&req.currency);
    let now = Utc::now();

    let metric = BillableMetric {
        id: Uuid::nil(),
        organization_id: Uuid::nil(),
        name: "estimate".into(),
        code: "estimate".into(),
        description: None,
        aggregation_type,
        field_name: req.field_name.clone(),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    };
    let events: Vec<Event> = req
        .events
        .into_iter()
        .map(|e| Event {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            customer_id: None,
            subscription_id: None,
            transaction_id: Uuid::new_v4().to_string(),
            code: "estimate".into(),
            properties: e.properties,
            external_customer_id: None,
            external_subscription_id: None,
            timestamp: None,
            precise_total_amount_cents: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            persisted: true,
        })
        .collect();

    let free_events = prop_i64(&req.properties, "free_units_per_events");
    let free_total = prop_decimal(&req.properties, "free_units_per_total_aggregation");
    let agg = aggregation::aggregate(&metric, &events, free_events, free_total)?;

    let ps = PricingStructure {
        charge_model,
        properties: req.properties,
        prorated: false,
        accepts_target_wallet: false,
        currency,
    };
    let cmr = charge_models::compute(&ps, &agg)?;
    Ok(Json(EstimateResponse {
        units: cmr.units.to_string(),
        amount: cmr.amount.to_string(),
        unit_amount: cmr.unit_amount.to_string(),
        amount_details: cmr.amount_details,
        events_count: cmr.count,
    }))
}

fn prop_i64(props: &Value, key: &str) -> i64 {
    match props.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}
fn prop_decimal(props: &Value, key: &str) -> rust_decimal::Decimal {
    use std::str::FromStr;
    match props.get(key) {
        Some(Value::String(s)) => rust_decimal::Decimal::from_str(s.trim()).unwrap_or_default(),
        Some(Value::Number(n)) => n
            .as_f64()
            .and_then(rust_decimal::Decimal::from_f64_retain)
            .unwrap_or_default(),
        _ => rust_decimal::Decimal::default(),
    }
}

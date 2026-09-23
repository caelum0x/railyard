//! GraphQL object types — faithful ports of `app/graphql/types/*` with Lago's
//! camelCase field names. Each wraps a `rago-core` model; money is exposed as
//! integer cents (`i64`) and decimals/timestamps as strings, matching Lago's
//! GraphQL scalars.

use async_graphql::{Object, SimpleObject, ID};
use chrono::{DateTime, Utc};
use rago_core::models as m;

fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}
fn ts_opt(dt: Option<DateTime<Utc>>) -> Option<String> {
    dt.map(|d| d.to_rfc3339())
}

/// `Types::Shared::CollectionMetadataType`.
#[derive(SimpleObject, Clone)]
pub struct CollectionMetadata {
    pub current_page: i64,
    pub next_page: Option<i64>,
    pub prev_page: Option<i64>,
    pub total_pages: i64,
    pub total_count: i64,
}

impl CollectionMetadata {
    pub fn build(page: i64, per_page: i64, total_count: i64) -> Self {
        let total_pages = if total_count > 0 {
            (total_count as f64 / per_page as f64).ceil() as i64
        } else {
            0
        };
        Self {
            current_page: if total_count > 0 { page } else { 0 },
            next_page: if page < total_pages { Some(page + 1) } else { None },
            prev_page: if page > 1 { Some(page - 1) } else { None },
            total_pages,
            total_count,
        }
    }
}

// ---- Organization ----------------------------------------------------------

pub struct Organization(pub m::Organization);

#[Object]
impl Organization {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn default_currency(&self) -> &str {
        &self.0.default_currency
    }
    async fn timezone(&self) -> &str {
        &self.0.timezone
    }
    async fn email(&self) -> Option<&str> {
        self.0.email.as_deref()
    }
    async fn country(&self) -> Option<&str> {
        self.0.country.as_deref()
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

// ---- Customer --------------------------------------------------------------

pub struct Customer(pub m::Customer);

#[Object]
impl Customer {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn external_id(&self) -> &str {
        &self.0.external_id
    }
    async fn name(&self) -> Option<&str> {
        self.0.name.as_deref()
    }
    async fn email(&self) -> Option<&str> {
        self.0.email.as_deref()
    }
    async fn country(&self) -> Option<&str> {
        self.0.country.as_deref()
    }
    async fn currency(&self) -> Option<&str> {
        self.0.currency.as_deref()
    }
    async fn timezone(&self) -> Option<&str> {
        self.0.timezone.as_deref()
    }
    async fn tax_identification_number(&self) -> Option<&str> {
        self.0.tax_identification_number.as_deref()
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
    async fn updated_at(&self) -> String {
        ts(self.0.updated_at)
    }
}

#[derive(SimpleObject)]
pub struct CustomerCollection {
    pub collection: Vec<Customer>,
    pub metadata: CollectionMetadata,
}

// ---- BillableMetric --------------------------------------------------------

pub struct BillableMetric(pub m::BillableMetric);

#[Object]
impl BillableMetric {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn code(&self) -> &str {
        &self.0.code
    }
    async fn description(&self) -> Option<&str> {
        self.0.description.as_deref()
    }
    async fn aggregation_type(&self) -> &str {
        self.0.aggregation_type.as_str()
    }
    async fn field_name(&self) -> Option<&str> {
        self.0.field_name.as_deref()
    }
    async fn recurring(&self) -> bool {
        self.0.recurring
    }
    async fn expression(&self) -> Option<&str> {
        self.0.expression.as_deref()
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

#[derive(SimpleObject)]
pub struct BillableMetricCollection {
    pub collection: Vec<BillableMetric>,
    pub metadata: CollectionMetadata,
}

// ---- Charge ----------------------------------------------------------------

pub struct Charge(pub m::Charge);

#[Object]
impl Charge {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn lago_billable_metric_id(&self) -> Option<ID> {
        self.0.billable_metric_id.map(Into::into)
    }
    async fn charge_model(&self) -> &str {
        self.0.charge_model.as_str()
    }
    async fn invoiceable(&self) -> bool {
        self.0.invoiceable
    }
    async fn pay_in_advance(&self) -> bool {
        self.0.pay_in_advance
    }
    async fn prorated(&self) -> bool {
        self.0.prorated
    }
    async fn min_amount_cents(&self) -> i64 {
        self.0.min_amount_cents
    }
    async fn properties(&self) -> async_graphql::Json<serde_json::Value> {
        async_graphql::Json(self.0.properties.clone())
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

// ---- Plan ------------------------------------------------------------------

pub struct Plan {
    pub plan: m::Plan,
    pub charges: Vec<m::Charge>,
}

#[Object]
impl Plan {
    async fn id(&self) -> ID {
        self.plan.id.into()
    }
    async fn name(&self) -> &str {
        &self.plan.name
    }
    async fn code(&self) -> &str {
        &self.plan.code
    }
    async fn interval(&self) -> Option<&str> {
        self.plan.interval.map(|i| i.as_str())
    }
    async fn description(&self) -> Option<&str> {
        self.plan.description.as_deref()
    }
    async fn amount_cents(&self) -> Option<i64> {
        self.plan.amount_cents
    }
    async fn amount_currency(&self) -> &str {
        &self.plan.amount_currency
    }
    async fn pay_in_advance(&self) -> bool {
        self.plan.pay_in_advance
    }
    async fn charges(&self) -> Vec<Charge> {
        self.charges.iter().cloned().map(Charge).collect()
    }
    async fn created_at(&self) -> String {
        ts(self.plan.created_at)
    }
}

#[derive(SimpleObject)]
pub struct PlanCollection {
    pub collection: Vec<Plan>,
    pub metadata: CollectionMetadata,
}

// ---- Subscription ----------------------------------------------------------

pub struct Subscription {
    pub sub: m::Subscription,
    pub plan_code: String,
    pub plan_amount_cents: Option<i64>,
    pub plan_amount_currency: String,
    pub external_customer_id: String,
}

#[Object]
impl Subscription {
    async fn id(&self) -> ID {
        self.sub.id.into()
    }
    async fn external_id(&self) -> &str {
        &self.sub.external_id
    }
    async fn lago_customer_id(&self) -> ID {
        self.sub.customer_id.into()
    }
    async fn external_customer_id(&self) -> &str {
        &self.external_customer_id
    }
    async fn name(&self) -> Option<&str> {
        self.sub.name.as_deref()
    }
    async fn plan_code(&self) -> &str {
        &self.plan_code
    }
    async fn plan_amount_cents(&self) -> Option<i64> {
        self.plan_amount_cents
    }
    async fn plan_amount_currency(&self) -> &str {
        &self.plan_amount_currency
    }
    async fn status(&self) -> &str {
        self.sub.status.as_str()
    }
    async fn billing_time(&self) -> &str {
        self.sub.billing_time.as_str()
    }
    async fn subscription_at(&self) -> Option<String> {
        ts_opt(self.sub.subscription_at)
    }
    async fn started_at(&self) -> Option<String> {
        ts_opt(self.sub.started_at)
    }
    async fn terminated_at(&self) -> Option<String> {
        ts_opt(self.sub.terminated_at)
    }
    async fn created_at(&self) -> String {
        ts(self.sub.created_at)
    }
}

#[derive(SimpleObject)]
pub struct SubscriptionCollection {
    pub collection: Vec<Subscription>,
    pub metadata: CollectionMetadata,
}

// ---- Fee / Invoice ---------------------------------------------------------

pub struct Fee(pub m::Fee);

#[Object]
impl Fee {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn lago_charge_id(&self) -> Option<ID> {
        self.0.charge_id.map(Into::into)
    }
    async fn fee_type(&self) -> Option<&str> {
        self.0.fee_type.map(|t| t.as_str())
    }
    async fn amount_cents(&self) -> i64 {
        self.0.amount_cents
    }
    async fn amount_currency(&self) -> &str {
        &self.0.amount_currency
    }
    async fn taxes_amount_cents(&self) -> i64 {
        self.0.taxes_amount_cents
    }
    async fn total_amount_cents(&self) -> i64 {
        self.0.amount_cents + self.0.taxes_amount_cents
    }
    async fn units(&self) -> String {
        self.0.units.to_string()
    }
    async fn events_count(&self) -> Option<i32> {
        self.0.events_count
    }
    async fn payment_status(&self) -> &str {
        self.0.payment_status.as_str()
    }
}

pub struct Invoice {
    pub invoice: m::Invoice,
    pub fees: Vec<m::Fee>,
}

#[Object]
impl Invoice {
    async fn id(&self) -> ID {
        self.invoice.id.into()
    }
    /// Download URL of the invoice PDF (rendered on request; authenticate
    /// with the organization's API key).
    async fn file_url(&self) -> String {
        rago_jobs::webhook_emit::document_url("invoices", self.invoice.id)
    }
    /// Download URL of the invoice's EN 16931 XML.
    async fn xml_url(&self) -> String {
        format!("{}.xml", rago_jobs::webhook_emit::document_url("invoices", self.invoice.id))
    }
    async fn number(&self) -> &str {
        &self.invoice.number
    }
    async fn sequential_id(&self) -> Option<i32> {
        self.invoice.sequential_id
    }
    async fn invoice_type(&self) -> &str {
        self.invoice.invoice_type.as_str()
    }
    async fn status(&self) -> &str {
        self.invoice.status.as_str()
    }
    async fn payment_status(&self) -> &str {
        self.invoice.payment_status.as_str()
    }
    async fn currency(&self) -> Option<&str> {
        self.invoice.currency.as_deref()
    }
    async fn issuing_date(&self) -> Option<String> {
        self.invoice.issuing_date.map(|d| d.format("%Y-%m-%d").to_string())
    }
    async fn fees_amount_cents(&self) -> i64 {
        self.invoice.fees_amount_cents
    }
    async fn taxes_amount_cents(&self) -> i64 {
        self.invoice.taxes_amount_cents
    }
    async fn coupons_amount_cents(&self) -> i64 {
        self.invoice.coupons_amount_cents
    }
    async fn credit_notes_amount_cents(&self) -> i64 {
        self.invoice.credit_notes_amount_cents
    }
    async fn prepaid_credit_amount_cents(&self) -> i64 {
        self.invoice.prepaid_credit_amount_cents
    }
    async fn sub_total_excluding_taxes_amount_cents(&self) -> i64 {
        self.invoice.sub_total_excluding_taxes_amount_cents
    }
    async fn sub_total_including_taxes_amount_cents(&self) -> i64 {
        self.invoice.sub_total_including_taxes_amount_cents
    }
    async fn total_amount_cents(&self) -> i64 {
        self.invoice.total_amount_cents
    }
    async fn fees(&self) -> Vec<Fee> {
        self.fees.iter().cloned().map(Fee).collect()
    }
    async fn created_at(&self) -> String {
        ts(self.invoice.created_at)
    }
}

#[derive(SimpleObject)]
pub struct InvoiceCollection {
    pub collection: Vec<Invoice>,
    pub metadata: CollectionMetadata,
}

// ---- Wallet ----------------------------------------------------------------

pub struct WalletTransaction(pub m::WalletTransaction);

#[Object]
impl WalletTransaction {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn lago_wallet_id(&self) -> ID {
        self.0.wallet_id.into()
    }
    async fn status(&self) -> &str {
        self.0.status.as_str()
    }
    async fn transaction_type(&self) -> &str {
        self.0.transaction_type.as_str()
    }
    async fn amount(&self) -> String {
        self.0.amount.to_string()
    }
    async fn credit_amount(&self) -> String {
        self.0.credit_amount.to_string()
    }
    async fn settled_at(&self) -> Option<String> {
        ts_opt(self.0.settled_at)
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

pub struct Wallet {
    pub wallet: m::Wallet,
    pub external_customer_id: String,
}

#[Object]
impl Wallet {
    async fn id(&self) -> ID {
        self.wallet.id.into()
    }
    async fn lago_customer_id(&self) -> ID {
        self.wallet.customer_id.into()
    }
    async fn external_customer_id(&self) -> &str {
        &self.external_customer_id
    }
    async fn status(&self) -> &str {
        self.wallet.status.as_str()
    }
    async fn currency(&self) -> &str {
        &self.wallet.balance_currency
    }
    async fn name(&self) -> Option<&str> {
        self.wallet.name.as_deref()
    }
    async fn rate_amount(&self) -> String {
        self.wallet.rate_amount.to_string()
    }
    async fn credits_balance(&self) -> String {
        self.wallet.credits_balance.to_string()
    }
    async fn balance_cents(&self) -> i64 {
        self.wallet.balance_cents
    }
    async fn consumed_credits(&self) -> String {
        self.wallet.consumed_credits.to_string()
    }
    async fn created_at(&self) -> String {
        ts(self.wallet.created_at)
    }
}

#[derive(SimpleObject)]
pub struct WalletCollection {
    pub collection: Vec<Wallet>,
    pub metadata: CollectionMetadata,
}

// ---- Coupon ----------------------------------------------------------------

pub struct Coupon(pub m::Coupon);

#[Object]
impl Coupon {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn code(&self) -> &str {
        &self.0.code
    }
    async fn coupon_type(&self) -> &str {
        self.0.coupon_type.as_str()
    }
    async fn amount_cents(&self) -> Option<i64> {
        self.0.amount_cents
    }
    async fn amount_currency(&self) -> Option<&str> {
        self.0.amount_currency.as_deref()
    }
    async fn percentage_rate(&self) -> Option<String> {
        self.0.percentage_rate.map(|d| d.to_string())
    }
    async fn frequency(&self) -> &str {
        self.0.frequency.as_str()
    }
    async fn frequency_duration(&self) -> Option<i32> {
        self.0.frequency_duration
    }
    async fn reusable(&self) -> bool {
        self.0.reusable
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

#[derive(SimpleObject)]
pub struct CouponCollection {
    pub collection: Vec<Coupon>,
    pub metadata: CollectionMetadata,
}

// ---- CreditNote ------------------------------------------------------------

pub struct CreditNote(pub m::CreditNote);

#[Object]
impl CreditNote {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    /// Download URL of the credit-note PDF (rendered on request).
    async fn file_url(&self) -> String {
        rago_jobs::webhook_emit::document_url("credit_notes", self.0.id)
    }
    async fn number(&self) -> &str {
        &self.0.number
    }
    async fn lago_invoice_id(&self) -> ID {
        self.0.invoice_id.into()
    }
    async fn currency(&self) -> &str {
        &self.0.credit_amount_currency
    }
    async fn total_amount_cents(&self) -> i64 {
        self.0.total_amount_cents
    }
    async fn credit_amount_cents(&self) -> i64 {
        self.0.credit_amount_cents
    }
    async fn refund_amount_cents(&self) -> i64 {
        self.0.refund_amount_cents
    }
    async fn balance_amount_cents(&self) -> i64 {
        self.0.balance_amount_cents
    }
    async fn taxes_amount_cents(&self) -> i64 {
        self.0.taxes_amount_cents
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

// ---- Analytics -------------------------------------------------------------

/// `Types::Analytics::*` — a monthly revenue/MRR/overdue data point.
#[derive(SimpleObject)]
pub struct AnalyticsDataPoint {
    pub month: String,
    pub currency: String,
    pub amount_cents: i64,
    pub count: i64,
}

impl From<rago_db::AnalyticsPoint> for AnalyticsDataPoint {
    fn from(p: rago_db::AnalyticsPoint) -> Self {
        Self {
            month: p.month,
            currency: p.currency,
            amount_cents: p.amount_cents,
            count: p.count,
        }
    }
}

/// `Types::Analytics::InvoiceCollection`.
#[derive(SimpleObject)]
pub struct InvoiceCollectionPoint {
    pub month: String,
    pub currency: String,
    pub payment_status: String,
    pub invoices_count: i64,
    pub amount_cents: i64,
}

impl From<rago_db::InvoiceCollectionPoint> for InvoiceCollectionPoint {
    fn from(p: rago_db::InvoiceCollectionPoint) -> Self {
        let payment_status = match p.payment_status {
            0 => "pending",
            1 => "succeeded",
            2 => "failed",
            _ => "unknown",
        }
        .to_string();
        Self {
            month: p.month,
            currency: p.currency,
            payment_status,
            invoices_count: p.invoices_count,
            amount_cents: p.amount_cents,
        }
    }
}

#[derive(SimpleObject)]
pub struct AnalyticsCollection {
    pub collection: Vec<AnalyticsDataPoint>,
}

#[derive(SimpleObject)]
pub struct InvoiceCollectionAnalytics {
    pub collection: Vec<InvoiceCollectionPoint>,
}

// ---- Customer portal usage -------------------------------------------------

/// `Types::Customers::Usage::ChargeUsage`.
#[derive(SimpleObject)]
pub struct ChargeUsage {
    pub units: String,
    pub amount_cents: i64,
    pub billable_metric_code: String,
    pub billable_metric_name: String,
}

/// `Types::Customers::Usage` — live current-period usage (not invoiced).
#[derive(SimpleObject)]
pub struct CustomerUsage {
    pub currency: String,
    pub amount_cents: i64,
    pub total_amount_cents: i64,
    pub charges_usage: Vec<ChargeUsage>,
}

// ---- Event -----------------------------------------------------------------

pub struct Event(pub m::Event);

#[Object]
impl Event {
    async fn id(&self) -> ID {
        self.0.id.into()
    }
    async fn transaction_id(&self) -> &str {
        &self.0.transaction_id
    }
    async fn code(&self) -> &str {
        &self.0.code
    }
    async fn external_subscription_id(&self) -> Option<&str> {
        self.0.external_subscription_id.as_deref()
    }
    async fn properties(&self) -> async_graphql::Json<serde_json::Value> {
        async_graphql::Json(self.0.properties.clone())
    }
    async fn timestamp(&self) -> Option<String> {
        self.0.timestamp.map(|t| t.and_utc().to_rfc3339())
    }
    async fn created_at(&self) -> String {
        ts(self.0.created_at)
    }
}

// ---- Relay pagination ------------------------------------------------------

/// `PageInfo` — Relay Connection page info.
#[derive(SimpleObject, Clone)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
}

/// Opaque cursor = base64("offset:<n>"), so `after` decodes to a 0-based index.
pub fn encode_cursor(offset: i64) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("offset:{offset}"))
}

/// Decode an opaque cursor to its 0-based offset, if valid.
pub fn decode_cursor(cursor: &str) -> Option<i64> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(cursor).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    s.strip_prefix("offset:")?.parse().ok()
}

fn page_info(start_offset: i64, count: usize, total: i64) -> PageInfo {
    let end_offset = start_offset + count as i64;
    PageInfo {
        has_next_page: end_offset < total,
        has_previous_page: start_offset > 0,
        start_cursor: (count > 0).then(|| encode_cursor(start_offset)),
        end_cursor: (count > 0).then(|| encode_cursor(end_offset - 1)),
    }
}

macro_rules! relay_connection {
    ($edge:ident, $conn:ident, $node:ty) => {
        #[derive(SimpleObject)]
        pub struct $edge {
            pub cursor: String,
            pub node: $node,
        }
        #[derive(SimpleObject)]
        pub struct $conn {
            pub edges: Vec<$edge>,
            pub page_info: PageInfo,
            pub total_count: i64,
        }
        impl $conn {
            /// Build a Relay connection from an already-sliced page of nodes
            /// beginning at `start_offset` within a collection of size `total`.
            pub fn build(nodes: Vec<$node>, start_offset: i64, total: i64) -> Self {
                let info = page_info(start_offset, nodes.len(), total);
                let edges = nodes
                    .into_iter()
                    .enumerate()
                    .map(|(i, node)| $edge {
                        cursor: encode_cursor(start_offset + i as i64),
                        node,
                    })
                    .collect();
                Self { edges, page_info: info, total_count: total }
            }
        }
    };
}

relay_connection!(CustomerEdge, CustomerConnection, Customer);
relay_connection!(SubscriptionEdge, SubscriptionConnection, Subscription);
relay_connection!(InvoiceEdge, InvoiceConnection, Invoice);
relay_connection!(EventEdge, EventConnection, Event);
relay_connection!(AddOnEdge, AddOnConnection, AddOn);

// ---- AddOn -----------------------------------------------------------------

/// `Types::AddOns::Object`.
#[derive(SimpleObject)]
pub struct AddOn {
    pub id: ID,
    pub name: String,
    pub code: String,
    pub amount_cents: i64,
    pub amount_currency: String,
}

// ---- UsageAlert ------------------------------------------------------------

/// `Types::UsageMonitoring::AlertObject`.
#[derive(SimpleObject)]
pub struct UsageAlert {
    pub id: ID,
    pub alert_type: String,
    pub billable_metric_code: Option<String>,
    pub threshold_cents: i64,
    pub recurring: bool,
    pub last_triggered_at: Option<String>,
}

// ---- dashboard admin types -------------------------------------------------

/// `Types::ApiKeys::Object`.
#[derive(SimpleObject)]
pub struct ApiKey {
    pub id: ID,
    pub name: Option<String>,
    pub value: String,
    pub created_at: String,
}

/// `Types::WebhookEndpoints::Object`.
#[derive(SimpleObject)]
pub struct WebhookEndpoint {
    pub id: ID,
    pub webhook_url: String,
    pub signature_algo: String,
}

/// `Types::Integrations::*` collection item.
#[derive(SimpleObject)]
pub struct Integration {
    pub id: ID,
    pub integration_type: String,
    pub provider_code: String,
    pub code: Option<String>,
    pub name: Option<String>,
}

impl From<rago_db::IntegrationRow> for Integration {
    fn from(r: rago_db::IntegrationRow) -> Self {
        Self {
            id: r.id.into(),
            integration_type: r.integration_type,
            provider_code: r.provider_code,
            code: r.code,
            name: r.name,
        }
    }
}

/// `Types::DunningCampaigns::Object`.
#[derive(SimpleObject)]
pub struct DunningCampaign {
    pub id: ID,
    pub name: String,
    pub code: String,
    pub max_attempts: i64,
    pub days_between_attempts: i64,
    pub applied_to_organization: bool,
}

/// `Types::LifetimeUsages::Object`.
#[derive(SimpleObject)]
pub struct LifetimeUsage {
    pub external_subscription_id: String,
    pub invoiced_usage_amount_cents: i64,
    pub current_usage_amount_cents: i64,
    pub total_usage_amount_cents: i64,
}

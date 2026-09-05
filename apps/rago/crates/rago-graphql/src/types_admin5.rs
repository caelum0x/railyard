//! GraphQL object types for slice-5 admin resources: quotes/orders, AI +
//! finance-assistant payloads, data exports, and the `data_api_*` analytics
//! data point.

use async_graphql::{SimpleObject, ID};

/// `Types::Quotes::Object`.
#[derive(SimpleObject)]
pub struct Quote {
    pub id: ID,
    pub lago_customer_id: ID,
    pub number: String,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub line_items: async_graphql::Json<serde_json::Value>,
}

impl From<rago_db::QuoteRow> for Quote {
    fn from(r: rago_db::QuoteRow) -> Self {
        Self {
            id: r.id.into(),
            lago_customer_id: r.customer_id.into(),
            number: r.number,
            status: r.status,
            currency: r.currency,
            total_amount_cents: r.total_amount_cents,
            line_items: async_graphql::Json(r.line_items),
        }
    }
}

/// `Types::QuoteVersions::Object`.
#[derive(SimpleObject)]
pub struct QuoteVersion {
    pub id: ID,
    pub lago_quote_id: ID,
    pub version_number: i32,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
}

impl From<rago_db::QuoteVersionRow> for QuoteVersion {
    fn from(r: rago_db::QuoteVersionRow) -> Self {
        Self {
            id: r.id.into(),
            lago_quote_id: r.quote_id.into(),
            version_number: r.version_number,
            status: r.status,
            currency: r.currency,
            total_amount_cents: r.total_amount_cents,
        }
    }
}

/// `Types::OrderForms::Object`.
#[derive(SimpleObject)]
pub struct OrderForm {
    pub id: ID,
    pub lago_customer_id: Option<ID>,
    pub number: String,
    pub status: String,
    pub execution_mode: Option<String>,
}

impl From<rago_db::OrderFormRow> for OrderForm {
    fn from(r: rago_db::OrderFormRow) -> Self {
        let (id, customer_id, number, status, execution_mode) = r;
        Self { id: id.into(), lago_customer_id: customer_id.map(Into::into), number, status, execution_mode }
    }
}

/// `Types::Orders::Object`.
#[derive(SimpleObject)]
pub struct Order {
    pub id: ID,
    pub lago_customer_id: Option<ID>,
    pub number: String,
    pub status: String,
}

impl From<rago_db::OrderRow> for Order {
    fn from(r: rago_db::OrderRow) -> Self {
        let (id, customer_id, number, status) = r;
        Self { id: id.into(), lago_customer_id: customer_id.map(Into::into), number, status }
    }
}

/// `Types::AiConversations::MessageObject`.
#[derive(SimpleObject)]
pub struct AiMessage {
    pub id: ID,
    pub role: String,
    pub content: String,
}

impl From<rago_db::AiMessageRow> for AiMessage {
    fn from(r: rago_db::AiMessageRow) -> Self {
        let (id, role, content) = r;
        Self { id: id.into(), role, content }
    }
}

/// `Types::AiConversations::Object`.
#[derive(SimpleObject)]
pub struct AiConversation {
    pub id: ID,
    pub name: Option<String>,
    pub messages: Vec<AiMessage>,
}

/// `Types::FinanceAssistant::Answer`.
#[derive(SimpleObject)]
pub struct FinanceAnswer {
    pub session_id: ID,
    pub question: String,
    pub answer: Option<String>,
    pub status: String,
}

/// `Types::DataExports::Object`.
#[derive(SimpleObject)]
pub struct DataExport {
    pub id: ID,
    pub resource_type: String,
    pub format: String,
    pub status: String,
    pub row_count: i32,
}

/// A `data_api_*` analytics data point (a keyed money amount).
#[derive(SimpleObject)]
pub struct DataApiPoint {
    pub key: String,
    pub currency: String,
    pub amount_cents: i64,
    pub count: i64,
}

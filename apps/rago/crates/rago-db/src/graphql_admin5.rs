//! Repository extensions for the GraphQL dashboard admin CRUD surface — slice 5
//! (final tail): orders/quotes, AI + finance-assistant persistence, data-export
//! filters, adjusted-fee revert, and the JWT → user-session resolution used by
//! the HTTP layer for `currentUser`.

use rago_core::models::Organization;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

#[derive(Debug, Clone)]
pub struct QuoteRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub number: String,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub line_items: Value,
}
fn quote_row(r: &sqlx::postgres::PgRow) -> QuoteRow {
    QuoteRow {
        id: r.get("id"),
        customer_id: r.get("customer_id"),
        number: r.get("number"),
        status: r.get("status"),
        currency: r.get("currency"),
        total_amount_cents: r.get("total_amount_cents"),
        line_items: r.get("line_items"),
    }
}

#[derive(Debug, Clone)]
pub struct QuoteVersionRow {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub version_number: i32,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
}
fn quote_version_row(r: &sqlx::postgres::PgRow) -> QuoteVersionRow {
    QuoteVersionRow {
        id: r.get("id"),
        quote_id: r.get("quote_id"),
        version_number: r.get("version_number"),
        status: r.get("status"),
        currency: r.get("currency"),
        total_amount_cents: r.get("total_amount_cents"),
    }
}

/// An order-form row `(id, customer_id, number, status, execution_mode)`.
pub type OrderFormRow = (Uuid, Option<Uuid>, String, String, Option<String>);
/// An order row `(id, customer_id, number, status)`.
pub type OrderRow = (Uuid, Option<Uuid>, String, String);
/// An AI message `(id, role, content)`.
pub type AiMessageRow = (Uuid, String, String);

#[derive(Debug, Clone)]
pub struct AiConversationRow {
    pub id: Uuid,
    pub name: Option<String>,
}

impl Db {
    // ---- quotes ----------------------------------------------------------

    pub async fn find_quote_full(&self, id: Uuid) -> Result<Option<QuoteRow>> {
        let row = sqlx::query("SELECT * FROM quotes WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(quote_row))
    }
    pub async fn list_quotes(&self, org: Uuid) -> Result<Vec<QuoteRow>> {
        let rows = sqlx::query("SELECT * FROM quotes WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(quote_row).collect())
    }
    pub async fn update_quote(&self, id: Uuid, status: Option<&str>, line_items: Option<&Value>, total_amount_cents: Option<i64>) -> Result<Option<QuoteRow>> {
        sqlx::query("UPDATE quotes SET status = COALESCE($2, status), line_items = COALESCE($3, line_items), total_amount_cents = COALESCE($4, total_amount_cents), updated_at = now() WHERE id = $1")
            .bind(id).bind(status).bind(line_items).bind(total_amount_cents).execute(&self.pool).await?;
        self.find_quote_full(id).await
    }

    // ---- quote versions --------------------------------------------------

    pub async fn insert_quote_version(&self, id: Uuid, org: Uuid, quote_id: Uuid, version_number: i32, currency: &str, total_amount_cents: i64, line_items: &Value) -> Result<QuoteVersionRow> {
        sqlx::query("INSERT INTO quote_versions (id, organization_id, quote_id, version_number, currency, total_amount_cents, line_items, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())")
            .bind(id).bind(org).bind(quote_id).bind(version_number).bind(currency).bind(total_amount_cents).bind(line_items).execute(&self.pool).await?;
        Ok(self.find_quote_version(id).await?.expect("just inserted"))
    }
    pub async fn find_quote_version(&self, id: Uuid) -> Result<Option<QuoteVersionRow>> {
        let row = sqlx::query("SELECT * FROM quote_versions WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(quote_version_row))
    }
    pub async fn list_quote_versions(&self, quote_id: Uuid) -> Result<Vec<QuoteVersionRow>> {
        let rows = sqlx::query("SELECT * FROM quote_versions WHERE quote_id = $1 ORDER BY version_number").bind(quote_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(quote_version_row).collect())
    }
    pub async fn update_quote_version(&self, id: Uuid, line_items: &Value, total_amount_cents: i64) -> Result<Option<QuoteVersionRow>> {
        sqlx::query("UPDATE quote_versions SET line_items = $2, total_amount_cents = $3, updated_at = now() WHERE id = $1 AND status = 'draft'")
            .bind(id).bind(line_items).bind(total_amount_cents).execute(&self.pool).await?;
        self.find_quote_version(id).await
    }
    pub async fn approve_quote_version(&self, id: Uuid, expires_at: Option<chrono::NaiveDateTime>) -> Result<Option<QuoteVersionRow>> {
        sqlx::query("UPDATE quote_versions SET status = 'approved', approved_at = now(), expires_at = $2, updated_at = now() WHERE id = $1")
            .bind(id).bind(expires_at).execute(&self.pool).await?;
        self.find_quote_version(id).await
    }
    pub async fn void_quote_version(&self, id: Uuid) -> Result<Option<QuoteVersionRow>> {
        sqlx::query("UPDATE quote_versions SET status = 'voided', voided_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.find_quote_version(id).await
    }
    /// Clone a quote version into a new draft version (next version number).
    pub async fn clone_quote_version(&self, id: Uuid) -> Result<Option<QuoteVersionRow>> {
        let Some(src) = sqlx::query("SELECT * FROM quote_versions WHERE id = $1").bind(id).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let quote_id: Uuid = src.get("quote_id");
        let org: Uuid = src.get("organization_id");
        let currency: String = src.get("currency");
        let total: i64 = src.get("total_amount_cents");
        let line_items: Value = src.get("line_items");
        let next: i32 = sqlx::query("SELECT COALESCE(MAX(version_number),0)+1 AS n FROM quote_versions WHERE quote_id = $1")
            .bind(quote_id).fetch_one(&self.pool).await?.get("n");
        Ok(Some(self.insert_quote_version(Uuid::new_v4(), org, quote_id, next, &currency, total, &line_items).await?))
    }

    // ---- order forms + orders -------------------------------------------

    pub async fn insert_order_form(&self, id: Uuid, org: Uuid, customer_id: Option<Uuid>, quote_id: Option<Uuid>, number: &str) -> Result<OrderFormRow> {
        sqlx::query("INSERT INTO order_forms (id, organization_id, customer_id, quote_id, number, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(org).bind(customer_id).bind(quote_id).bind(number).execute(&self.pool).await?;
        Ok(self.find_order_form(id).await?.expect("just inserted"))
    }
    pub async fn find_order_form(&self, id: Uuid) -> Result<Option<OrderFormRow>> {
        let row = sqlx::query("SELECT id, customer_id, number, status, execution_mode FROM order_forms WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"), r.get("execution_mode"))))
    }
    pub async fn list_order_forms(&self, org: Uuid) -> Result<Vec<OrderFormRow>> {
        let rows = sqlx::query("SELECT id, customer_id, number, status, execution_mode FROM order_forms WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"), r.get("execution_mode"))).collect())
    }
    pub async fn mark_order_form_signed(&self, id: Uuid, signed_document: Option<&str>, execution_mode: Option<&str>, execute_at: Option<chrono::NaiveDateTime>) -> Result<Option<OrderFormRow>> {
        sqlx::query("UPDATE order_forms SET status = 'signed', signed_at = now(), signed_document = $2, execution_mode = $3, execute_at = $4, updated_at = now() WHERE id = $1")
            .bind(id).bind(signed_document).bind(execution_mode).bind(execute_at).execute(&self.pool).await?;
        self.find_order_form(id).await
    }
    pub async fn void_order_form(&self, id: Uuid) -> Result<Option<OrderFormRow>> {
        sqlx::query("UPDATE order_forms SET status = 'voided', updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.find_order_form(id).await
    }

    pub async fn insert_order(&self, id: Uuid, org: Uuid, customer_id: Option<Uuid>, order_form_id: Option<Uuid>, number: &str) -> Result<OrderRow> {
        sqlx::query("INSERT INTO orders (id, organization_id, customer_id, order_form_id, number, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(org).bind(customer_id).bind(order_form_id).bind(number).execute(&self.pool).await?;
        Ok(self.find_order(id).await?.expect("just inserted"))
    }
    pub async fn find_order(&self, id: Uuid) -> Result<Option<OrderRow>> {
        let row = sqlx::query("SELECT id, customer_id, number, status FROM orders WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"))))
    }
    pub async fn list_orders(&self, org: Uuid) -> Result<Vec<OrderRow>> {
        let rows = sqlx::query("SELECT id, customer_id, number, status FROM orders WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("customer_id"), r.get("number"), r.get("status"))).collect())
    }
    pub async fn execute_order(&self, id: Uuid) -> Result<Option<OrderRow>> {
        sqlx::query("UPDATE orders SET status = 'executed', executed_at = now(), updated_at = now() WHERE id = $1 AND status = 'pending'").bind(id).execute(&self.pool).await?;
        self.find_order(id).await
    }
    pub async fn update_order_status(&self, id: Uuid, status: &str) -> Result<Option<OrderRow>> {
        sqlx::query("UPDATE orders SET status = $2, updated_at = now() WHERE id = $1").bind(id).bind(status).execute(&self.pool).await?;
        self.find_order(id).await
    }

    // ---- AI conversations + finance assistant ---------------------------

    pub async fn insert_ai_conversation(&self, id: Uuid, org: Uuid, membership_id: Option<Uuid>, name: Option<&str>) -> Result<()> {
        sqlx::query("INSERT INTO ai_conversations (id, organization_id, membership_id, name, created_at, updated_at) VALUES ($1,$2,$3,$4,now(),now())")
            .bind(id).bind(org).bind(membership_id).bind(name).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn find_ai_conversation(&self, id: Uuid) -> Result<Option<AiConversationRow>> {
        let row = sqlx::query("SELECT id, name FROM ai_conversations WHERE id = $1").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| AiConversationRow { id: r.get("id"), name: r.get("name") }))
    }
    pub async fn list_ai_conversations(&self, org: Uuid) -> Result<Vec<AiConversationRow>> {
        let rows = sqlx::query("SELECT id, name FROM ai_conversations WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| AiConversationRow { id: r.get("id"), name: r.get("name") }).collect())
    }
    pub async fn insert_ai_message(&self, id: Uuid, org: Uuid, conversation_id: Uuid, role: &str, content: &str) -> Result<()> {
        sqlx::query("INSERT INTO ai_messages (id, organization_id, ai_conversation_id, role, content, created_at) VALUES ($1,$2,$3,$4,$5,now())")
            .bind(id).bind(org).bind(conversation_id).bind(role).bind(content).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list_ai_messages(&self, conversation_id: Uuid) -> Result<Vec<AiMessageRow>> {
        let rows = sqlx::query("SELECT id, role, content FROM ai_messages WHERE ai_conversation_id = $1 ORDER BY created_at").bind(conversation_id).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("role"), r.get("content"))).collect())
    }
    pub async fn insert_finance_assistant_message(&self, id: Uuid, org: Uuid, session_id: Uuid, question: &str, answer: Option<&str>, status: &str) -> Result<()> {
        sqlx::query("INSERT INTO finance_assistant_messages (id, organization_id, session_id, question, answer, status, created_at) VALUES ($1,$2,$3,$4,$5,$6,now())")
            .bind(id).bind(org).bind(session_id).bind(question).bind(answer).bind(status).execute(&self.pool).await?;
        Ok(())
    }

    // ---- data exports (with filters) ------------------------------------

    pub async fn insert_data_export_filtered(&self, id: Uuid, org: Uuid, resource_type: &str, format: &str, filters: &Value) -> Result<()> {
        sqlx::query("INSERT INTO data_exports (id, organization_id, resource_type, format, status, filters, created_at, updated_at) VALUES ($1,$2,$3,$4,'pending',$5,now(),now())")
            .bind(id).bind(org).bind(resource_type).bind(format).bind(filters).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list_data_exports(&self, org: Uuid) -> Result<Vec<(Uuid, String, String, String, i32)>> {
        let rows = sqlx::query("SELECT id, resource_type, format, status, row_count FROM data_exports WHERE organization_id = $1 ORDER BY created_at DESC").bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("resource_type"), r.get("format"), r.get("status"), r.get("row_count"))).collect())
    }

    // ---- adjusted fee revert --------------------------------------------

    /// Record the pre-adjustment values on an adjusted-fee row (so a later
    /// destroy can restore the fee).
    pub async fn set_adjusted_fee_original(&self, adjusted_fee_id: Uuid, units: Decimal, amount_cents: i64, unit_amount_cents: i64) -> Result<()> {
        sqlx::query("UPDATE adjusted_fees SET original_units = $2, original_amount_cents = $3, original_unit_amount_cents = $4 WHERE id = $1")
            .bind(adjusted_fee_id).bind(units).bind(amount_cents).bind(unit_amount_cents).execute(&self.pool).await?;
        Ok(())
    }
    /// The `(fee_id, original_units, original_amount_cents, original_unit_amount_cents)`
    /// captured for an adjusted fee, if any.
    pub async fn adjusted_fee_original(&self, id: Uuid) -> Result<Option<(Option<Uuid>, Option<Decimal>, Option<i64>, Option<i64>)>> {
        let row = sqlx::query("SELECT fee_id, original_units, original_amount_cents, original_unit_amount_cents FROM adjusted_fees WHERE id = $1")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| (r.get("fee_id"), r.get("original_units"), r.get("original_amount_cents"), r.get("original_unit_amount_cents"))))
    }

    // ---- data_api analytics helpers -------------------------------------

    /// Prepaid-credit balances by wallet currency `(currency, amount_cents)`.
    pub async fn prepaid_credit_balances(&self, org: Uuid) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query("SELECT balance_currency AS currency, COALESCE(SUM(balance_cents),0)::bigint AS amount FROM wallets WHERE organization_id = $1 AND status = 0 GROUP BY balance_currency")
            .bind(org).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (r.get::<String, _>("currency"), r.get::<i64, _>("amount"))).collect())
    }

    // ---- JWT → user session resolution (HTTP layer) ---------------------

    /// Resolve a user id to `(organization, email)` via their first active
    /// membership — used by the HTTP layer to build a dashboard session for
    /// `currentUser`.
    pub async fn resolve_user_session(&self, user_id: Uuid) -> Result<Option<(Organization, String)>> {
        let Some((_, email)) = self.find_user(user_id).await? else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT o.* FROM organizations o JOIN memberships m ON m.organization_id = o.id \
             WHERE m.user_id = $1 AND m.status = 'active' ORDER BY m.created_at LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some((mapping::organization(&r)?, email))),
            None => Ok(None),
        }
    }
}

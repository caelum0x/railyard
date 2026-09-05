//! GraphQL admin read queries — slice 5. Quotes/orders, AI conversations, data
//! exports, and the `data_api_*` analytics queries. The `data_api_*` fields that
//! Rago can compute from persisted invoice/fee/wallet data return real numbers;
//! the usage-snapshot-dependent ones return honest-empty (they require the
//! daily-usage snapshots produced by the deferred `ComputeAllDailyUsages` clock
//! job — noted as an infra dependency).

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{db, require_org};
use crate::types_admin5::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

fn points(rows: Vec<rago_db::AnalyticsPoint>) -> Vec<DataApiPoint> {
    rows.into_iter().map(|p| DataApiPoint { key: p.month, currency: p.currency, amount_cents: p.amount_cents, count: p.count }).collect()
}

/// Admin query root — slice 5 (merged into the schema's `Query`).
#[derive(Default)]
pub struct AdminQuery5;

#[Object]
impl AdminQuery5 {
    // ---- quotes / orders -------------------------------------------------

    async fn quotes(&self, ctx: &Context<'_>) -> Result<Vec<Quote>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_quotes(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn quote(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Quote>> {
        require_org(ctx)?;
        Ok(db(ctx).find_quote_full(uuid(&id)?).await?.map(Into::into))
    }
    async fn quote_versions(&self, ctx: &Context<'_>, quote_id: ID) -> Result<Vec<QuoteVersion>> {
        require_org(ctx)?;
        Ok(db(ctx).list_quote_versions(uuid(&quote_id)?).await?.into_iter().map(Into::into).collect())
    }
    async fn order_forms(&self, ctx: &Context<'_>) -> Result<Vec<OrderForm>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_order_forms(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn order_form(&self, ctx: &Context<'_>, id: ID) -> Result<Option<OrderForm>> {
        require_org(ctx)?;
        Ok(db(ctx).find_order_form(uuid(&id)?).await?.map(Into::into))
    }
    async fn orders(&self, ctx: &Context<'_>) -> Result<Vec<Order>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_orders(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn order(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Order>> {
        require_org(ctx)?;
        Ok(db(ctx).find_order(uuid(&id)?).await?.map(Into::into))
    }

    // ---- AI + data exports ----------------------------------------------

    async fn ai_conversations(&self, ctx: &Context<'_>) -> Result<Vec<AiConversation>> {
        let org = require_org(ctx)?;
        let mut out = Vec::new();
        for c in db(ctx).list_ai_conversations(org.id).await? {
            let messages = db(ctx).list_ai_messages(c.id).await?.into_iter().map(Into::into).collect();
            out.push(AiConversation { id: c.id.into(), name: c.name, messages });
        }
        Ok(out)
    }
    async fn ai_conversation(&self, ctx: &Context<'_>, id: ID) -> Result<Option<AiConversation>> {
        require_org(ctx)?;
        let cid = uuid(&id)?;
        let Some(c) = db(ctx).find_ai_conversation(cid).await? else {
            return Ok(None);
        };
        let messages = db(ctx).list_ai_messages(cid).await?.into_iter().map(Into::into).collect();
        Ok(Some(AiConversation { id: c.id.into(), name: c.name, messages }))
    }
    async fn data_exports(&self, ctx: &Context<'_>) -> Result<Vec<DataExport>> {
        let org = require_org(ctx)?;
        Ok(db(ctx)
            .list_data_exports(org.id)
            .await?
            .into_iter()
            .map(|(id, resource_type, format, status, row_count)| DataExport { id: id.into(), resource_type, format, status, row_count })
            .collect())
    }

    // ---- data_api_* analytics -------------------------------------------

    /// `data_api_mrrs` — monthly recurring revenue (real, from subscription fees).
    async fn data_api_mrrs(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        let org = require_org(ctx)?;
        Ok(points(db(ctx).mrrs(org.id).await?))
    }

    /// `data_api_revenue_streams` — finalized-invoice revenue by month (real).
    async fn data_api_revenue_streams(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        let org = require_org(ctx)?;
        Ok(points(db(ctx).gross_revenues(org.id).await?))
    }

    /// `data_api_prepaid_credits` — outstanding prepaid wallet balances (real).
    async fn data_api_prepaid_credits(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        let org = require_org(ctx)?;
        Ok(db(ctx)
            .prepaid_credit_balances(org.id)
            .await?
            .into_iter()
            .map(|(currency, amount_cents)| DataApiPoint { key: "prepaid_credits".into(), currency, amount_cents, count: 0 })
            .collect())
    }

    // The following require per-plan/per-customer daily-usage snapshots
    // (ComputeAllDailyUsages — a deferred clock job). They return honest-empty
    // until that infra lands, rather than fabricated numbers.
    async fn data_api_mrrs_plans(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_revenue_streams_customers(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_revenue_streams_plans(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_usages(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_usages_aggregated_amounts(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_usages_forecasted(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
    async fn data_api_usages_invoiced(&self, ctx: &Context<'_>) -> Result<Vec<DataApiPoint>> {
        require_org(ctx)?;
        Ok(Vec::new())
    }
}

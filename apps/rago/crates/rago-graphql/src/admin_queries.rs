//! GraphQL admin read queries — the list/show surface for the resources the
//! admin mutations manage (taxes, add-ons, single plan/metric/subscription/
//! wallet/coupon/credit-note/dunning/webhook-endpoint lookups + charge filters).
//! Merged into the schema's `Query`. Every resolver is gated behind the
//! authenticated organization and delegates to the shared `rago-db` repositories.

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{db, require_org};
use crate::types::*;
use crate::types_admin::*;

const PER_PAGE: i64 = 100;

fn page(page: Option<i64>) -> i64 {
    page.filter(|p| *p > 0).unwrap_or(1)
}
fn limit(per_page: Option<i64>) -> i64 {
    per_page.filter(|p| *p > 0).unwrap_or(PER_PAGE)
}
fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

/// Admin query root (merged into the schema's `Query`).
#[derive(Default)]
pub struct AdminQuery;

#[Object]
impl AdminQuery {
    // ---- taxes -----------------------------------------------------------

    /// `taxes` — the org's taxes.
    async fn taxes(&self, ctx: &Context<'_>, page: Option<i64>, per_page: Option<i64>) -> Result<TaxCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let mut items = db(ctx).list_taxes(org.id).await?;
        let total = items.len() as i64;
        let start = ((pg - 1) * lim).max(0) as usize;
        let end = (start + lim as usize).min(items.len());
        let slice = if start < items.len() { items.drain(start..end).collect() } else { Vec::new() };
        Ok(TaxCollection {
            collection: slice.into_iter().map(Tax).collect(),
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `tax(id:)`.
    async fn tax(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Tax>> {
        require_org(ctx)?;
        Ok(db(ctx).find_tax(uuid(&id)?).await?.map(Tax))
    }

    // ---- add-ons ---------------------------------------------------------

    /// `addOns` — paginated add-ons.
    async fn add_ons(&self, ctx: &Context<'_>, page: Option<i64>, per_page: Option<i64>) -> Result<AddOnCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let (rows, total) = db(ctx).list_add_ons(org.id, lim, (pg - 1) * lim).await?;
        Ok(AddOnCollection {
            collection: rows
                .into_iter()
                .map(|(id, name, code, amount_cents, currency)| AddOn {
                    id: id.into(),
                    name,
                    code,
                    amount_cents,
                    amount_currency: currency,
                })
                .collect(),
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `addOn(id:)`.
    async fn add_on(&self, ctx: &Context<'_>, id: ID) -> Result<Option<AddOn>> {
        require_org(ctx)?;
        Ok(db(ctx).find_add_on(uuid(&id)?).await?.map(|(id, name, code, _desc, amount_cents, currency)| AddOn {
            id: id.into(),
            name,
            code,
            amount_cents,
            amount_currency: currency,
        }))
    }

    // ---- single-resource lookups ----------------------------------------

    /// `billableMetric(code:)`.
    async fn billable_metric(&self, ctx: &Context<'_>, code: String) -> Result<Option<BillableMetric>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).find_metric_by_code(org.id, &code).await?.map(BillableMetric))
    }

    /// `coupon(code:)`.
    async fn coupon(&self, ctx: &Context<'_>, code: String) -> Result<Option<Coupon>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).find_coupon_by_code(org.id, &code).await?.map(Coupon))
    }

    /// `subscription(id:)`.
    async fn subscription(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Subscription>> {
        let org = require_org(ctx)?;
        let Some(sub) = db(ctx).find_subscription_opt(uuid(&id)?).await? else {
            return Ok(None);
        };
        if sub.organization_id != org.id {
            return Ok(None);
        }
        let plan = db(ctx).find_plan(sub.plan_id).await?;
        let ext = db(ctx).customer_external_id(sub.customer_id).await?.unwrap_or_default();
        Ok(Some(Subscription {
            plan_code: plan.code.clone(),
            plan_amount_cents: plan.amount_cents,
            plan_amount_currency: plan.amount_currency.clone(),
            external_customer_id: ext,
            sub,
        }))
    }

    /// `wallet(id:)`.
    async fn wallet(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Wallet>> {
        require_org(ctx)?;
        let Some(wallet) = db(ctx).find_wallet(uuid(&id)?).await? else {
            return Ok(None);
        };
        let ext = db(ctx).customer_external_id(wallet.customer_id).await?.unwrap_or_default();
        Ok(Some(Wallet { wallet, external_customer_id: ext }))
    }

    /// `dunningCampaign(id:)`.
    async fn dunning_campaign(&self, ctx: &Context<'_>, id: ID) -> Result<Option<DunningCampaign>> {
        require_org(ctx)?;
        Ok(db(ctx).find_dunning_campaign(uuid(&id)?).await?.map(|(id, name, code, _d, max, days, applied)| {
            DunningCampaign {
                id: id.into(),
                name,
                code,
                max_attempts: max as i64,
                days_between_attempts: days as i64,
                applied_to_organization: applied,
            }
        }))
    }

    /// `webhookEndpoint(id:)`.
    async fn webhook_endpoint(&self, ctx: &Context<'_>, id: ID) -> Result<Option<WebhookEndpoint>> {
        require_org(ctx)?;
        Ok(db(ctx)
            .find_webhook_endpoint(uuid(&id)?)
            .await?
            .map(|(id, webhook_url, signature_algo)| WebhookEndpoint {
                id: id.into(),
                webhook_url,
                signature_algo,
            }))
    }

    // ---- credit notes + charge filters ----------------------------------

    /// `creditNotes(invoiceId:)` — credit notes issued against an invoice.
    async fn credit_notes(&self, ctx: &Context<'_>, invoice_id: ID) -> Result<Vec<CreditNote>> {
        require_org(ctx)?;
        Ok(db(ctx)
            .credit_notes_for_invoice(uuid(&invoice_id)?)
            .await?
            .into_iter()
            .map(CreditNote)
            .collect())
    }

    /// `chargeFilters(chargeId:)` — the charge's filters.
    async fn charge_filters(&self, ctx: &Context<'_>, charge_id: ID) -> Result<Vec<ChargeFilter>> {
        require_org(ctx)?;
        Ok(db(ctx)
            .charge_filters_for_charge(uuid(&charge_id)?)
            .await?
            .into_iter()
            .map(ChargeFilter)
            .collect())
    }
}

//! GraphQL admin read queries — slice 2. List/show for billing entities,
//! invoice custom sections, fixed charges, usage-monitoring alerts (subscription
//! + wallet), features, payments, and subscription entitlements. Merged into the
//! schema's `Query`; every resolver is gated behind the authenticated org.

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{db, require_org};
use crate::types_admin2::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

async fn alerts_for(ctx: &Context<'_>, alertable_type: &str, id: Uuid) -> Result<Vec<Alert>> {
    let rows = db(ctx).alerts_for(alertable_type, id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let thresholds = db(ctx).alert_thresholds(row.id).await?;
        out.push(Alert::build(row, thresholds));
    }
    Ok(out)
}

/// Admin query root — slice 2 (merged into the schema's `Query`).
#[derive(Default)]
pub struct AdminQuery2;

#[Object]
impl AdminQuery2 {
    // ---- billing entities -----------------------------------------------

    /// `billingEntities`.
    async fn billing_entities(&self, ctx: &Context<'_>) -> Result<Vec<BillingEntity>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_billing_entities(org.id).await?.into_iter().map(Into::into).collect())
    }

    /// `billingEntity(id:)`.
    async fn billing_entity(&self, ctx: &Context<'_>, id: ID) -> Result<Option<BillingEntity>> {
        require_org(ctx)?;
        Ok(db(ctx).find_billing_entity(uuid(&id)?).await?.map(Into::into))
    }

    // ---- invoice custom sections ----------------------------------------

    /// `invoiceCustomSections`.
    async fn invoice_custom_sections(&self, ctx: &Context<'_>) -> Result<Vec<InvoiceCustomSection>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_invoice_custom_sections(org.id).await?.into_iter().map(Into::into).collect())
    }

    /// `invoiceCustomSection(id:)`.
    async fn invoice_custom_section(&self, ctx: &Context<'_>, id: ID) -> Result<Option<InvoiceCustomSection>> {
        require_org(ctx)?;
        Ok(db(ctx).find_invoice_custom_section(uuid(&id)?).await?.map(Into::into))
    }

    // ---- fixed charges ---------------------------------------------------

    /// `fixedCharges(planId:)`.
    async fn fixed_charges(&self, ctx: &Context<'_>, plan_id: ID) -> Result<Vec<FixedCharge>> {
        require_org(ctx)?;
        Ok(db(ctx).fixed_charges_for_plan(uuid(&plan_id)?).await?.into_iter().map(Into::into).collect())
    }

    // ---- alerts ----------------------------------------------------------

    /// `subscriptionAlerts(subscriptionId:)`.
    async fn subscription_alerts(&self, ctx: &Context<'_>, subscription_id: ID) -> Result<Vec<Alert>> {
        require_org(ctx)?;
        alerts_for(ctx, "Subscription", uuid(&subscription_id)?).await
    }

    /// `walletAlerts(walletId:)`.
    async fn wallet_alerts(&self, ctx: &Context<'_>, wallet_id: ID) -> Result<Vec<Alert>> {
        require_org(ctx)?;
        alerts_for(ctx, "Wallet", uuid(&wallet_id)?).await
    }

    /// `alert(id:)`.
    async fn alert(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Alert>> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        let Some(row) = db(ctx).find_alert(uid).await? else {
            return Ok(None);
        };
        let thresholds = db(ctx).alert_thresholds(uid).await?;
        Ok(Some(Alert::build(row, thresholds)))
    }

    // ---- features + payments + entitlements -----------------------------

    /// `features`.
    async fn features(&self, ctx: &Context<'_>) -> Result<Vec<Feature>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_features(org.id).await?.into_iter().map(Into::into).collect())
    }

    /// `feature(code:)`.
    async fn feature(&self, ctx: &Context<'_>, code: String) -> Result<Option<Feature>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).find_feature(org.id, &code).await?.map(Into::into))
    }

    /// `payments(invoiceId:)`.
    async fn payments(&self, ctx: &Context<'_>, invoice_id: ID) -> Result<Vec<Payment>> {
        require_org(ctx)?;
        Ok(db(ctx).list_payments(uuid(&invoice_id)?).await?.into_iter().map(Into::into).collect())
    }

    /// `subscriptionEntitlements(subscriptionId:)`.
    async fn subscription_entitlements(&self, ctx: &Context<'_>, subscription_id: ID) -> Result<Vec<Entitlement>> {
        require_org(ctx)?;
        Ok(db(ctx).subscription_entitlements(uuid(&subscription_id)?).await?.into_iter().map(Into::into).collect())
    }
}

//! GraphQL admin read queries — slice 4. List/show for catalog v2 (products,
//! categories, filters, rate cards + rates, plan-applied rate cards, rate
//! phases, pricing units, commitments) and RBAC (memberships, roles, invites,
//! currentUser). Merged into the schema's `Query`.

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{current_user, db, require_org};
use crate::types_admin4::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

/// Admin query root — slice 4 (merged into the schema's `Query`).
#[derive(Default)]
pub struct AdminQuery4;

#[Object]
impl AdminQuery4 {
    // ---- catalog: products ----------------------------------------------

    async fn products(&self, ctx: &Context<'_>) -> Result<Vec<Product>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_products(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn product(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Product>> {
        require_org(ctx)?;
        Ok(db(ctx).find_product(uuid(&id)?).await?.map(Into::into))
    }
    async fn product_categories(&self, ctx: &Context<'_>) -> Result<Vec<ProductCategory>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_product_categories(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn product_filters(&self, ctx: &Context<'_>, product_id: ID) -> Result<Vec<ProductFilter>> {
        require_org(ctx)?;
        Ok(db(ctx).list_product_filters(uuid(&product_id)?).await?.into_iter().map(Into::into).collect())
    }

    // ---- catalog: rate cards --------------------------------------------

    async fn rate_cards(&self, ctx: &Context<'_>) -> Result<Vec<RateCard>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_rate_cards(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn rate_card(&self, ctx: &Context<'_>, id: ID) -> Result<Option<RateCard>> {
        require_org(ctx)?;
        Ok(db(ctx).find_rate_card(uuid(&id)?).await?.map(Into::into))
    }
    async fn rate_card_rates(&self, ctx: &Context<'_>, rate_card_id: ID) -> Result<Vec<RateCardRate>> {
        require_org(ctx)?;
        Ok(db(ctx).list_rate_card_rates(uuid(&rate_card_id)?).await?.into_iter().map(Into::into).collect())
    }
    async fn plan_applied_rate_cards(&self, ctx: &Context<'_>, plan_id: ID) -> Result<Vec<PlanAppliedRateCard>> {
        require_org(ctx)?;
        Ok(db(ctx).list_plan_applied_rate_cards(uuid(&plan_id)?).await?.into_iter().map(Into::into).collect())
    }
    async fn rate_phases(&self, ctx: &Context<'_>, plan_applied_rate_card_id: ID) -> Result<Vec<RatePhase>> {
        require_org(ctx)?;
        Ok(db(ctx).list_rate_phases(uuid(&plan_applied_rate_card_id)?).await?.into_iter().map(Into::into).collect())
    }

    // ---- catalog: pricing units + commitments ---------------------------

    async fn pricing_units(&self, ctx: &Context<'_>) -> Result<Vec<PricingUnit>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_pricing_units(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn commitments(&self, ctx: &Context<'_>, plan_id: ID) -> Result<Vec<Commitment>> {
        require_org(ctx)?;
        Ok(db(ctx).list_commitments(uuid(&plan_id)?).await?.into_iter().map(Into::into).collect())
    }

    // ---- RBAC ------------------------------------------------------------

    async fn memberships(&self, ctx: &Context<'_>) -> Result<Vec<Membership>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_memberships(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn roles(&self, ctx: &Context<'_>) -> Result<Vec<Role>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_roles(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn role(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Role>> {
        require_org(ctx)?;
        Ok(db(ctx).find_role(uuid(&id)?).await?.map(Into::into))
    }
    async fn invites(&self, ctx: &Context<'_>) -> Result<Vec<Invite>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_invites(org.id).await?.into_iter().map(Into::into).collect())
    }
    async fn invite(&self, ctx: &Context<'_>, token: String) -> Result<Option<Invite>> {
        require_org(ctx)?;
        Ok(db(ctx).find_invite_by_token(&token).await?.map(Into::into))
    }

    /// `currentUser` — the authenticated dashboard user, resolved by the HTTP
    /// layer from a session JWT (`UserSession`). API-key requests carry only an
    /// organization principal, so this returns `null` for them.
    async fn current_user(&self, ctx: &Context<'_>) -> Result<Option<User>> {
        require_org(ctx)?;
        Ok(current_user(ctx).map(|u| User { id: u.id.into(), email: u.email.clone() }))
    }
}

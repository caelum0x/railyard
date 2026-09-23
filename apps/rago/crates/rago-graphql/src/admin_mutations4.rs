//! GraphQL admin mutations — slice 4: auth/RBAC (register/login/password-reset,
//! memberships, roles, invites), API-key rotate/destroy, billing-entity taxes,
//! and catalog v2 (products, rate cards, pricing units, commitments).
//!
//! Passwords are hashed with argon2 (`crate::auth`) and only the digest is
//! persisted; login issues a signed HS256 JWT. No plaintext, ever.

use async_graphql::{Context, Object, Result, ID};
use chrono::Utc;
use rago_core::enums::ChargeModel;
use rago_core::models as m;
use serde_json::json;
use uuid::Uuid;

use crate::admin_inputs4::*;
use crate::auth;
use crate::context::{db, require_org};
use crate::types::ApiKey;
use crate::types_admin::Tax;
use crate::types_admin2::BillingEntity;
use crate::types_admin4::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}
fn charge_model_i32(s: &str) -> Option<i32> {
    Some(
        match s {
            "standard" => ChargeModel::Standard,
            "graduated" => ChargeModel::Graduated,
            "package" => ChargeModel::Package,
            "percentage" => ChargeModel::Percentage,
            "volume" => ChargeModel::Volume,
            "graduated_percentage" => ChargeModel::GraduatedPercentage,
            "custom" => ChargeModel::Custom,
            "dynamic" => ChargeModel::Dynamic,
            _ => return None,
        }
        .as_i32(),
    )
}

/// Admin CRUD mutation root — slice 4 (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation4;

#[Object]
impl AdminMutation4 {
    // ---- auth ------------------------------------------------------------

    /// `RegisterUser` — create a user + organization + owner membership.
    async fn register_user(&self, ctx: &Context<'_>, email: String, password: String, organization_name: String) -> Result<LoginPayload> {
        if db(ctx).find_user_by_email(&email).await?.is_some() {
            return Err(async_graphql::Error::new("email_already_taken"));
        }
        let digest = auth::hash_password(&password).map_err(async_graphql::Error::new)?;
        let now = Utc::now();
        let org = m::Organization {
            id: Uuid::new_v4(),
            name: organization_name.clone(),
            slug: format!("{}-{}", organization_name.to_lowercase().replace(' ', "-"), &Uuid::new_v4().to_string()[..8]),
            default_currency: "USD".into(),
            email: Some(email.clone()),
            country: None,
            tax_identification_number: None,
            timezone: "UTC".into(),
            document_locale: "en".into(),
            finalize_zero_amount_invoice: true,
            hmac_key: Some(Uuid::new_v4().to_string()),
            created_at: now,
            updated_at: now,
        };
        db(ctx).insert_organization(&org).await?;
        let user_id = Uuid::new_v4();
        db(ctx).insert_user(user_id, &email, Some(&digest)).await?;
        db(ctx).insert_membership(Uuid::new_v4(), org.id, user_id, "admin", &json!([])).await?;
        Ok(LoginPayload { token: auth::issue_jwt(user_id), user: User { id: user_id.into(), email } })
    }

    /// `LoginUser` — verify credentials and open a session.
    async fn login_user(&self, ctx: &Context<'_>, email: String, password: String) -> Result<LoginPayload> {
        let (user_id, user_email, digest) = db(ctx)
            .find_user_by_email(&email)
            .await?
            .ok_or_else(|| async_graphql::Error::new("incorrect_login_or_password"))?;
        let digest = digest.ok_or_else(|| async_graphql::Error::new("incorrect_login_or_password"))?;
        if !auth::verify_password(&password, &digest) {
            return Err(async_graphql::Error::new("incorrect_login_or_password"));
        }
        // Organizations that enforce SSO disable password sign-in for their
        // members (checked after the password so it never leaks membership).
        let allowed = rago_jobs::sso::password_login_allowed(db(ctx), user_id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        if !allowed {
            return Err(async_graphql::Error::new("sso_required"));
        }
        Ok(LoginPayload { token: auth::issue_jwt(user_id), user: User { id: user_id.into(), email: user_email } })
    }

    /// `CreatePasswordReset` — mint a reset token for the user (returned as the
    /// payload `id`; Lago emails it out of band).
    async fn create_password_reset(&self, ctx: &Context<'_>, email: String) -> Result<ID> {
        let Some((user_id, _, _)) = db(ctx).find_user_by_email(&email).await? else {
            // Do not leak whether the email exists.
            return Ok(Uuid::new_v4().into());
        };
        let id = Uuid::new_v4();
        let token = auth::random_token();
        db(ctx).insert_password_reset(id, user_id, &token).await?;
        Ok(id.into())
    }

    /// `ResetPassword` — set a new password from a valid reset token and log in.
    async fn reset_password(&self, ctx: &Context<'_>, token: String, new_password: String) -> Result<LoginPayload> {
        let (reset_id, user_id) = db(ctx)
            .find_password_reset_by_token(&token)
            .await?
            .ok_or_else(|| async_graphql::Error::new("invalid_or_expired_token"))?;
        let digest = auth::hash_password(&new_password).map_err(async_graphql::Error::new)?;
        db(ctx).update_user_password(user_id, &digest).await?;
        db(ctx).delete_password_reset(reset_id).await?;
        let (_, email) = db(ctx).find_user(user_id).await?.ok_or_else(|| async_graphql::Error::new("user_not_found"))?;
        Ok(LoginPayload { token: auth::issue_jwt(user_id), user: User { id: user_id.into(), email } })
    }

    // ---- memberships -----------------------------------------------------

    /// `UpdateMembership` — set a membership's custom roles.
    async fn update_membership(&self, ctx: &Context<'_>, id: ID, roles: Vec<String>) -> Result<Membership> {
        require_org(ctx)?;
        let row = db(ctx).update_membership_roles(uuid(&id)?, &json!(roles)).await?.ok_or_else(|| async_graphql::Error::new("membership_not_found"))?;
        Ok(row.into())
    }

    /// `RevokeMembership`.
    async fn revoke_membership(&self, ctx: &Context<'_>, id: ID) -> Result<Membership> {
        require_org(ctx)?;
        let row = db(ctx).revoke_membership(uuid(&id)?).await?.ok_or_else(|| async_graphql::Error::new("membership_not_found"))?;
        Ok(row.into())
    }

    // ---- roles -----------------------------------------------------------

    /// `CreateRole`.
    async fn create_role(&self, ctx: &Context<'_>, input: CreateRoleInput) -> Result<Role> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_role(Uuid::new_v4(), org.id, &input.code, &input.name, input.description.as_deref(), &json!(input.permissions)).await?;
        Ok(row.into())
    }

    /// `UpdateRole`.
    async fn update_role(&self, ctx: &Context<'_>, input: UpdateRoleInput) -> Result<Role> {
        require_org(ctx)?;
        let permissions = input.permissions.map(|p| json!(p));
        let row = db(ctx).update_role(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), permissions.as_ref()).await?.ok_or_else(|| async_graphql::Error::new("role_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyRole`.
    async fn destroy_role(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_role(uid).await?;
        Ok(uid.into())
    }

    // ---- invites ---------------------------------------------------------

    /// `CreateInvite`.
    async fn create_invite(&self, ctx: &Context<'_>, email: String, roles: Vec<String>) -> Result<Invite> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_invite(Uuid::new_v4(), org.id, &email, &auth::random_token(), &json!(roles)).await?;
        Ok(row.into())
    }

    /// `UpdateInvite`.
    async fn update_invite(&self, ctx: &Context<'_>, id: ID, roles: Vec<String>) -> Result<Invite> {
        require_org(ctx)?;
        let row = db(ctx).update_invite_roles(uuid(&id)?, &json!(roles)).await?.ok_or_else(|| async_graphql::Error::new("invite_not_found"))?;
        Ok(row.into())
    }

    /// `RevokeInvite`.
    async fn revoke_invite(&self, ctx: &Context<'_>, id: ID) -> Result<Invite> {
        require_org(ctx)?;
        let row = db(ctx).revoke_invite(uuid(&id)?).await?.ok_or_else(|| async_graphql::Error::new("invite_not_found"))?;
        Ok(row.into())
    }

    /// `AcceptInvite` — create the user + membership from a pending invite and
    /// log in. Unauthenticated (the invitee has no session yet).
    async fn accept_invite(&self, ctx: &Context<'_>, email: String, password: String, token: String) -> Result<LoginPayload> {
        let invite = db(ctx).find_invite_by_token(&token).await?.ok_or_else(|| async_graphql::Error::new("invite_not_found"))?;
        if invite.status != "pending" {
            return Err(async_graphql::Error::new("invite_not_pending"));
        }
        let user_id = match db(ctx).find_user_by_email(&email).await? {
            Some((id, _, _)) => id,
            None => {
                let digest = auth::hash_password(&password).map_err(async_graphql::Error::new)?;
                let id = Uuid::new_v4();
                db(ctx).insert_user(id, &email, Some(&digest)).await?;
                id
            }
        };
        db(ctx).insert_membership(Uuid::new_v4(), invite.organization_id, user_id, "manager", &invite.roles).await?;
        db(ctx).mark_invite_accepted(invite.id).await?;
        Ok(LoginPayload { token: auth::issue_jwt(user_id), user: User { id: user_id.into(), email } })
    }

    // ---- api keys --------------------------------------------------------

    /// `RotateApiKey` — expire the old key and mint a replacement.
    async fn rotate_api_key(&self, ctx: &Context<'_>, id: ID, name: Option<String>) -> Result<ApiKey> {
        let org = require_org(ctx)?;
        let (new_id, value, name) = db(ctx).rotate_api_key(org.id, uuid(&id)?, name.as_deref()).await?.ok_or_else(|| async_graphql::Error::new("api_key_not_found"))?;
        Ok(ApiKey { id: new_id.into(), name, value, created_at: Utc::now().to_rfc3339() })
    }

    /// `DestroyApiKey`.
    async fn destroy_api_key(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        let org = require_org(ctx)?;
        let uid = uuid(&id)?;
        if !db(ctx).destroy_api_key(org.id, uid).await? {
            return Err(async_graphql::Error::new("api_key_not_found"));
        }
        Ok(uid.into())
    }

    // ---- billing entity taxes -------------------------------------------

    /// `BillingEntities::ApplyTaxes` — attach taxes (by code) to a billing
    /// entity; returns the applied taxes.
    async fn apply_billing_entity_taxes(&self, ctx: &Context<'_>, billing_entity_id: ID, tax_codes: Vec<String>) -> Result<Vec<Tax>> {
        let org = require_org(ctx)?;
        let be = uuid(&billing_entity_id)?;
        for code in &tax_codes {
            if let Some(tax) = db(ctx).find_tax_by_code(org.id, code).await? {
                db(ctx).apply_billing_entity_tax(org.id, be, tax.id).await?;
            }
        }
        Ok(db(ctx).billing_entity_taxes(be).await?.into_iter().map(Tax).collect())
    }

    /// `BillingEntities::RemoveTaxes`.
    async fn remove_billing_entity_taxes(&self, ctx: &Context<'_>, billing_entity_id: ID, tax_codes: Vec<String>) -> Result<Vec<Tax>> {
        let org = require_org(ctx)?;
        let be = uuid(&billing_entity_id)?;
        for code in &tax_codes {
            if let Some(tax) = db(ctx).find_tax_by_code(org.id, code).await? {
                db(ctx).remove_billing_entity_tax(be, tax.id).await?;
            }
        }
        Ok(db(ctx).billing_entity_taxes(be).await?.into_iter().map(Tax).collect())
    }

    /// `BillingEntityUpdateAppliedDunningCampaign`.
    async fn billing_entity_update_applied_dunning_campaign(&self, ctx: &Context<'_>, billing_entity_id: ID, applied_dunning_campaign_id: Option<ID>) -> Result<BillingEntity> {
        require_org(ctx)?;
        let campaign = match &applied_dunning_campaign_id {
            Some(c) => Some(uuid(c)?),
            None => None,
        };
        let row = db(ctx).update_billing_entity_dunning(uuid(&billing_entity_id)?, campaign).await?.ok_or_else(|| async_graphql::Error::new("billing_entity_not_found"))?;
        Ok(row.into())
    }

    // ---- catalog: products ----------------------------------------------

    async fn create_product(&self, ctx: &Context<'_>, input: CreateProductInput) -> Result<Product> {
        let org = require_org(ctx)?;
        let bm = match &input.billable_metric_id {
            Some(b) => Some(uuid(b)?),
            None => None,
        };
        let cat = match &input.product_category_id {
            Some(c) => Some(uuid(c)?),
            None => None,
        };
        let row = db(ctx)
            .insert_product(Uuid::new_v4(), org.id, &input.code, &input.name, input.description.as_deref(), input.invoice_display_name.as_deref(), Some(&input.product_type), bm, cat)
            .await?;
        Ok(row.into())
    }
    async fn update_product(&self, ctx: &Context<'_>, input: UpdateProductInput) -> Result<Product> {
        require_org(ctx)?;
        let cat = match &input.product_category_id {
            Some(c) => Some(uuid(c)?),
            None => None,
        };
        let row = db(ctx).update_product(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), input.invoice_display_name.as_deref(), cat).await?.ok_or_else(|| async_graphql::Error::new("product_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_product(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_product(uid).await?;
        Ok(uid.into())
    }

    // ---- catalog: product categories ------------------------------------

    async fn create_product_category(&self, ctx: &Context<'_>, input: CreateProductCategoryInput) -> Result<ProductCategory> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_product_category(Uuid::new_v4(), org.id, &input.code, &input.name, input.description.as_deref(), input.invoice_display_name.as_deref()).await?;
        Ok(row.into())
    }
    async fn update_product_category(&self, ctx: &Context<'_>, input: UpdateProductCategoryInput) -> Result<ProductCategory> {
        require_org(ctx)?;
        let row = db(ctx).update_product_category(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), input.invoice_display_name.as_deref()).await?.ok_or_else(|| async_graphql::Error::new("product_category_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_product_category(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_product_category(uid).await?;
        Ok(uid.into())
    }

    // ---- catalog: product filters ---------------------------------------

    async fn create_product_filter(&self, ctx: &Context<'_>, input: CreateProductFilterInput) -> Result<ProductFilter> {
        let org = require_org(ctx)?;
        let row = db(ctx)
            .insert_product_filter(Uuid::new_v4(), org.id, uuid(&input.product_id)?, &input.code, &input.name, input.description.as_deref(), input.invoice_display_name.as_deref(), &input.values.0)
            .await?;
        Ok(row.into())
    }
    async fn update_product_filter(&self, ctx: &Context<'_>, input: UpdateProductFilterInput) -> Result<ProductFilter> {
        require_org(ctx)?;
        let values = input.values.map(|j| j.0);
        let row = db(ctx).update_product_filter(uuid(&input.id)?, input.name.as_deref(), input.invoice_display_name.as_deref(), values.as_ref()).await?.ok_or_else(|| async_graphql::Error::new("product_filter_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_product_filter(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_product_filter(uid).await?;
        Ok(uid.into())
    }

    // ---- catalog: rate cards + rates ------------------------------------

    async fn create_rate_card(&self, ctx: &Context<'_>, input: CreateRateCardInput) -> Result<RateCard> {
        let org = require_org(ctx)?;
        let row = db(ctx)
            .insert_rate_card(
                Uuid::new_v4(),
                org.id,
                uuid(&input.product_id)?,
                &input.code,
                &input.name,
                input.description.as_deref(),
                &input.currency,
                input.billing_timing.as_deref(),
                input.proration.unwrap_or(false),
                input.display_on_invoice.unwrap_or(true),
                input.wallet_targetable.unwrap_or(false),
            )
            .await?;
        Ok(row.into())
    }
    async fn update_rate_card(&self, ctx: &Context<'_>, input: UpdateRateCardInput) -> Result<RateCard> {
        require_org(ctx)?;
        let row = db(ctx).update_rate_card(uuid(&input.id)?, input.name.as_deref(), input.description.as_deref(), input.proration, input.display_on_invoice, input.wallet_targetable).await?.ok_or_else(|| async_graphql::Error::new("rate_card_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_rate_card(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_rate_card(uid).await?;
        Ok(uid.into())
    }

    async fn create_rate_card_rate(&self, ctx: &Context<'_>, input: CreateRateCardRateInput) -> Result<RateCardRate> {
        let org = require_org(ctx)?;
        let charge_model = charge_model_i32(&input.charge_model).ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?;
        let row = db(ctx).insert_rate_card_rate(Uuid::new_v4(), org.id, uuid(&input.rate_card_id)?, charge_model, &input.properties.map(|j| j.0).unwrap_or_else(|| json!({}))).await?;
        Ok(row.into())
    }
    async fn update_rate_card_rate(&self, ctx: &Context<'_>, input: UpdateRateCardRateInput) -> Result<RateCardRate> {
        require_org(ctx)?;
        let charge_model = match input.charge_model.as_deref() {
            Some(s) => Some(charge_model_i32(s).ok_or_else(|| async_graphql::Error::new("invalid charge_model"))?),
            None => None,
        };
        let props = input.properties.map(|j| j.0);
        let row = db(ctx).update_rate_card_rate(uuid(&input.id)?, charge_model, props.as_ref()).await?.ok_or_else(|| async_graphql::Error::new("rate_card_rate_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_rate_card_rate(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_rate_card_rate(uid).await?;
        Ok(uid.into())
    }

    // ---- catalog: plan-applied rate cards + rate phases -----------------

    async fn create_plan_applied_rate_card(&self, ctx: &Context<'_>, input: CreatePlanAppliedRateCardInput) -> Result<PlanAppliedRateCard> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_plan_applied_rate_card(Uuid::new_v4(), org.id, uuid(&input.plan_id)?, uuid(&input.rate_card_id)?).await?;
        Ok(row.into())
    }
    async fn destroy_plan_applied_rate_card(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_plan_applied_rate_card(uid).await?;
        Ok(uid.into())
    }

    async fn create_rate_phase(&self, ctx: &Context<'_>, input: CreateRatePhaseInput) -> Result<RatePhase> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_rate_phase(Uuid::new_v4(), org.id, uuid(&input.plan_applied_rate_card_id)?, &input.code, input.name.as_deref(), input.position, input.billing_interval_cycle_count).await?;
        Ok(row.into())
    }
    async fn update_rate_phase(&self, ctx: &Context<'_>, input: UpdateRatePhaseInput) -> Result<RatePhase> {
        require_org(ctx)?;
        let row = db(ctx).update_rate_phase(uuid(&input.id)?, input.name.as_deref(), input.position, input.billing_interval_cycle_count).await?.ok_or_else(|| async_graphql::Error::new("rate_phase_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_rate_phase(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_rate_phase(uid).await?;
        Ok(uid.into())
    }

    // ---- catalog: pricing units + commitments ---------------------------

    async fn create_pricing_unit(&self, ctx: &Context<'_>, input: CreatePricingUnitInput) -> Result<PricingUnit> {
        let org = require_org(ctx)?;
        let row = db(ctx).insert_pricing_unit(Uuid::new_v4(), org.id, &input.code, &input.name, &input.short_name, input.description.as_deref()).await?;
        Ok(row.into())
    }
    async fn update_pricing_unit(&self, ctx: &Context<'_>, input: UpdatePricingUnitInput) -> Result<PricingUnit> {
        require_org(ctx)?;
        let row = db(ctx).update_pricing_unit(uuid(&input.id)?, input.name.as_deref(), input.short_name.as_deref(), input.description.as_deref()).await?.ok_or_else(|| async_graphql::Error::new("pricing_unit_not_found"))?;
        Ok(row.into())
    }

    async fn create_commitment(&self, ctx: &Context<'_>, input: CreateCommitmentInput) -> Result<Commitment> {
        let org = require_org(ctx)?;
        let plan = match &input.plan_id {
            Some(p) => Some(uuid(p)?),
            None => None,
        };
        let row = db(ctx).insert_commitment(Uuid::new_v4(), org.id, plan, input.commitment_type.as_deref().unwrap_or("minimum_commitment"), input.amount_cents, input.invoice_display_name.as_deref()).await?;
        Ok(row.into())
    }
    async fn update_commitment(&self, ctx: &Context<'_>, input: UpdateCommitmentInput) -> Result<Commitment> {
        require_org(ctx)?;
        let row = db(ctx).update_commitment(uuid(&input.id)?, input.amount_cents, input.invoice_display_name.as_deref()).await?.ok_or_else(|| async_graphql::Error::new("commitment_not_found"))?;
        Ok(row.into())
    }
    async fn destroy_commitment(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_commitment(uid).await?;
        Ok(uid.into())
    }
}

//! GraphQL object types for slice-4 admin resources: auth/RBAC (user, login/
//! register payloads, membership, role, invite) and catalog v2 (product,
//! product category/filter, rate card/rate, plan-applied rate card, rate phase,
//! pricing unit, commitment).

use async_graphql::{SimpleObject, ID};
use rago_core::enums::ChargeModel;
use serde_json::Value;

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

// ---- auth / RBAC -----------------------------------------------------------

/// `Types::UserType`.
#[derive(SimpleObject, Clone)]
pub struct User {
    pub id: ID,
    pub email: String,
}

/// `Types::Payloads::LoginUserType` / `RegisterUserType` — `{ token, user }`.
#[derive(SimpleObject)]
pub struct LoginPayload {
    pub token: String,
    pub user: User,
}

/// A started SSO login: send the browser to `url`; the IdP redirects back
/// with `code` + `state` for the matching `*Login` mutation.
#[derive(SimpleObject)]
pub struct SsoAuthorization {
    pub url: String,
    pub state: String,
    pub provider: String,
}

/// `Types::MembershipType`.
#[derive(SimpleObject)]
pub struct Membership {
    pub id: ID,
    pub lago_user_id: ID,
    pub role: String,
    pub roles: Vec<String>,
    pub status: String,
}

impl From<rago_db::MembershipRow> for Membership {
    fn from(r: rago_db::MembershipRow) -> Self {
        Self { id: r.id.into(), lago_user_id: r.user_id.into(), role: r.role, roles: str_vec(&r.roles), status: r.status }
    }
}

/// `Types::RoleType`.
#[derive(SimpleObject)]
pub struct Role {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
}

impl From<rago_db::RoleRow> for Role {
    fn from(r: rago_db::RoleRow) -> Self {
        Self { id: r.id.into(), code: r.code, name: r.name, description: r.description, permissions: str_vec(&r.permissions) }
    }
}

/// `Types::Invites::Object`.
#[derive(SimpleObject)]
pub struct Invite {
    pub id: ID,
    pub email: String,
    pub token: String,
    pub roles: Vec<String>,
    pub status: String,
}

impl From<rago_db::InviteRow> for Invite {
    fn from(r: rago_db::InviteRow) -> Self {
        Self { id: r.id.into(), email: r.email, token: r.token, roles: str_vec(&r.roles), status: r.status }
    }
}

// ---- catalog v2 ------------------------------------------------------------

/// `Types::Products::Object`.
#[derive(SimpleObject)]
pub struct Product {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
    pub product_type: Option<String>,
    pub lago_billable_metric_id: Option<ID>,
    pub lago_product_category_id: Option<ID>,
}

impl From<rago_db::ProductRow> for Product {
    fn from(r: rago_db::ProductRow) -> Self {
        Self {
            id: r.id.into(),
            code: r.code,
            name: r.name,
            description: r.description,
            invoice_display_name: r.invoice_display_name,
            product_type: r.product_type,
            lago_billable_metric_id: r.billable_metric_id.map(Into::into),
            lago_product_category_id: r.product_category_id.map(Into::into),
        }
    }
}

/// `Types::ProductCategories::Object`.
#[derive(SimpleObject)]
pub struct ProductCategory {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub invoice_display_name: Option<String>,
}

impl From<rago_db::ProductCategoryRow> for ProductCategory {
    fn from(r: rago_db::ProductCategoryRow) -> Self {
        let (id, code, name, description, invoice_display_name) = r;
        Self { id: id.into(), code, name, description, invoice_display_name }
    }
}

/// `Types::ProductFilters::Object`.
#[derive(SimpleObject)]
pub struct ProductFilter {
    pub id: ID,
    pub lago_product_id: ID,
    pub code: String,
    pub name: String,
    pub invoice_display_name: Option<String>,
    pub values: async_graphql::Json<Value>,
}

impl From<rago_db::ProductFilterRow> for ProductFilter {
    fn from(r: rago_db::ProductFilterRow) -> Self {
        Self {
            id: r.id.into(),
            lago_product_id: r.product_id.into(),
            code: r.code,
            name: r.name,
            invoice_display_name: r.invoice_display_name,
            values: async_graphql::Json(r.values),
        }
    }
}

/// `Types::RateCards::Object`.
#[derive(SimpleObject)]
pub struct RateCard {
    pub id: ID,
    pub lago_product_id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub currency: String,
    pub billing_timing: Option<String>,
    pub proration: bool,
    pub display_on_invoice: bool,
    pub wallet_targetable: bool,
}

impl From<rago_db::RateCardRow> for RateCard {
    fn from(r: rago_db::RateCardRow) -> Self {
        Self {
            id: r.id.into(),
            lago_product_id: r.product_id.into(),
            code: r.code,
            name: r.name,
            description: r.description,
            currency: r.currency,
            billing_timing: r.billing_timing,
            proration: r.proration,
            display_on_invoice: r.display_on_invoice,
            wallet_targetable: r.wallet_targetable,
        }
    }
}

/// `Types::RateCardRates::Object`.
#[derive(SimpleObject)]
pub struct RateCardRate {
    pub id: ID,
    pub lago_rate_card_id: ID,
    pub charge_model: String,
    pub properties: async_graphql::Json<Value>,
}

impl From<rago_db::RateCardRateRow> for RateCardRate {
    fn from(r: rago_db::RateCardRateRow) -> Self {
        let (id, rate_card_id, charge_model, properties) = r;
        let charge_model = ChargeModel::try_from(charge_model).map(|m| m.as_str()).unwrap_or("standard").to_string();
        Self { id: id.into(), lago_rate_card_id: rate_card_id.into(), charge_model, properties: async_graphql::Json(properties) }
    }
}

/// `Types::PlanAppliedRateCards::Object`.
#[derive(SimpleObject)]
pub struct PlanAppliedRateCard {
    pub id: ID,
    pub lago_plan_id: ID,
    pub lago_rate_card_id: ID,
}

impl From<rago_db::PlanAppliedRateCardRow> for PlanAppliedRateCard {
    fn from(r: rago_db::PlanAppliedRateCardRow) -> Self {
        let (id, plan_id, rate_card_id) = r;
        Self { id: id.into(), lago_plan_id: plan_id.into(), lago_rate_card_id: rate_card_id.into() }
    }
}

/// `Types::RatePhases::Object`.
#[derive(SimpleObject)]
pub struct RatePhase {
    pub id: ID,
    pub lago_plan_applied_rate_card_id: ID,
    pub code: String,
    pub name: Option<String>,
    pub position: Option<i32>,
    pub billing_interval_cycle_count: Option<i32>,
}

impl From<rago_db::RatePhaseRow> for RatePhase {
    fn from(r: rago_db::RatePhaseRow) -> Self {
        Self {
            id: r.id.into(),
            lago_plan_applied_rate_card_id: r.plan_applied_rate_card_id.into(),
            code: r.code,
            name: r.name,
            position: r.position,
            billing_interval_cycle_count: r.billing_interval_cycle_count,
        }
    }
}

/// `Types::PricingUnits::Object`.
#[derive(SimpleObject)]
pub struct PricingUnit {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub short_name: String,
    pub description: Option<String>,
}

impl From<rago_db::PricingUnitRow> for PricingUnit {
    fn from(r: rago_db::PricingUnitRow) -> Self {
        let (id, code, name, short_name, description) = r;
        Self { id: id.into(), code, name, short_name, description }
    }
}

/// `Types::Commitments::Object`.
#[derive(SimpleObject)]
pub struct Commitment {
    pub id: ID,
    pub lago_plan_id: Option<ID>,
    pub commitment_type: String,
    pub amount_cents: i64,
    pub invoice_display_name: Option<String>,
}

impl From<rago_db::CommitmentRow> for Commitment {
    fn from(r: rago_db::CommitmentRow) -> Self {
        let (id, plan_id, commitment_type, amount_cents, invoice_display_name) = r;
        Self { id: id.into(), lago_plan_id: plan_id.map(Into::into), commitment_type, amount_cents, invoice_display_name }
    }
}

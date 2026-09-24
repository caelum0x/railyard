//! Progressive billing on the GraphQL dashboard API: usage thresholds on plans
//! (inherited by their subscriptions) and per-subscription overrides — the
//! GraphQL counterpart of REST `GET|PUT /plans/:code/usage_thresholds` and
//! `/subscriptions/:external_id/usage_thresholds`.
//!
//! * `Plan.usageThresholds` / `Subscription.usageThresholds` (in force)
//! * `usageThresholds(planId | subscriptionId)` query
//! * `updatePlanUsageThresholds` / `updateSubscriptionUsageThresholds`
//!   mutations (replace the whole set; an empty list clears it — for a
//!   subscription that falls back to the plan's thresholds)
//! * `createPlan(input: { usageThresholds })` / `updatePlan(input: {
//!   usageThresholds })`

use async_graphql::{Context, InputObject, Object, Result, SimpleObject, ID};
use rago_db::progressive::{validate_thresholds, NewUsageThreshold, ThresholdOwner, UsageThresholdRow};
use uuid::Uuid;

use crate::context::{db, require_org};

/// Longest accepted threshold display name.
const MAX_DISPLAY_NAME: usize = 255;

/// `Types::UsageThresholds::Object`.
#[derive(SimpleObject, Clone)]
pub struct UsageThreshold {
    pub id: ID,
    pub threshold_display_name: Option<String>,
    pub amount_cents: i64,
    pub recurring: bool,
    pub created_at: String,
}

impl From<&UsageThresholdRow> for UsageThreshold {
    fn from(t: &UsageThresholdRow) -> Self {
        Self {
            id: t.id.into(),
            threshold_display_name: t.threshold_display_name.clone(),
            amount_cents: t.amount_cents,
            recurring: t.recurring,
            created_at: t.created_at.to_rfc3339(),
        }
    }
}

/// `Types::UsageThresholds::Input`.
#[derive(InputObject, Clone)]
pub struct UsageThresholdInput {
    pub threshold_display_name: Option<String>,
    pub amount_cents: i64,
    pub recurring: Option<bool>,
}

/// The thresholds in force for a plan or subscription.
#[derive(SimpleObject)]
pub struct UsageThresholdSet {
    pub usage_thresholds: Vec<UsageThreshold>,
    /// `true` when a subscription has no override and uses its plan's set.
    pub inherited_from_plan: bool,
}

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

/// Validate GraphQL threshold inputs into the persistence shape.
pub(crate) fn parse_threshold_inputs(items: &[UsageThresholdInput]) -> Result<Vec<NewUsageThreshold>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let name = item
            .threshold_display_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        if name.as_deref().is_some_and(|n| n.len() > MAX_DISPLAY_NAME) {
            return Err(async_graphql::Error::new("threshold_display_name_too_long"));
        }
        out.push(NewUsageThreshold {
            threshold_display_name: name,
            amount_cents: item.amount_cents,
            recurring: item.recurring.unwrap_or(false),
        });
    }
    validate_thresholds(&out).map_err(|code| async_graphql::Error::new(code))?;
    Ok(out)
}

/// Replace a plan's threshold set (validated), returning the new set.
pub(crate) async fn replace_plan_thresholds(
    ctx: &Context<'_>,
    org: Uuid,
    plan_id: Uuid,
    items: &[UsageThresholdInput],
) -> Result<Vec<UsageThreshold>> {
    let parsed = parse_threshold_inputs(items)?;
    let rows = db(ctx).replace_usage_thresholds(org, ThresholdOwner::Plan(plan_id), &parsed).await?;
    Ok(rows.iter().map(UsageThreshold::from).collect())
}

/// A plan's own thresholds.
pub(crate) async fn plan_thresholds(ctx: &Context<'_>, plan_id: Uuid) -> Result<Vec<UsageThreshold>> {
    let rows = db(ctx).usage_thresholds_for(ThresholdOwner::Plan(plan_id)).await?;
    Ok(rows.iter().map(UsageThreshold::from).collect())
}

/// The thresholds in force for a subscription (own override, else its plan's).
pub(crate) async fn subscription_thresholds(
    ctx: &Context<'_>,
    subscription_id: Uuid,
    plan_id: Uuid,
) -> Result<UsageThresholdSet> {
    let own = db(ctx).usage_thresholds_for(ThresholdOwner::Subscription(subscription_id)).await?;
    if !own.is_empty() {
        return Ok(UsageThresholdSet {
            usage_thresholds: own.iter().map(UsageThreshold::from).collect(),
            inherited_from_plan: false,
        });
    }
    let inherited = db(ctx).usage_thresholds_for(ThresholdOwner::Plan(plan_id)).await?;
    Ok(UsageThresholdSet {
        usage_thresholds: inherited.iter().map(UsageThreshold::from).collect(),
        inherited_from_plan: true,
    })
}

/// Load a plan of the caller's organization.
async fn org_plan(ctx: &Context<'_>, org: Uuid, id: &ID) -> Result<rago_core::models::Plan> {
    let plan = db(ctx)
        .find_plan(uuid(id)?)
        .await
        .map_err(|_| async_graphql::Error::new("plan_not_found"))?;
    if plan.organization_id != org || plan.deleted_at.is_some() {
        return Err(async_graphql::Error::new("plan_not_found"));
    }
    Ok(plan)
}

/// Load a subscription of the caller's organization.
async fn org_subscription(ctx: &Context<'_>, org: Uuid, id: &ID) -> Result<rago_core::models::Subscription> {
    db(ctx)
        .find_subscription_opt(uuid(id)?)
        .await?
        .filter(|s| s.organization_id == org)
        .ok_or_else(|| async_graphql::Error::new("subscription_not_found"))
}

/// Usage-threshold query root (merged into the schema's `Query`).
#[derive(Default)]
pub struct ThresholdQuery;

#[Object]
impl ThresholdQuery {
    /// The thresholds of a plan (`planId`) or in force for a subscription
    /// (`subscriptionId`). Exactly one of the two is required.
    async fn usage_thresholds(
        &self,
        ctx: &Context<'_>,
        plan_id: Option<ID>,
        subscription_id: Option<ID>,
    ) -> Result<UsageThresholdSet> {
        let org = require_org(ctx)?.id;
        match (plan_id, subscription_id) {
            (Some(p), None) => {
                let plan = org_plan(ctx, org, &p).await?;
                Ok(UsageThresholdSet { usage_thresholds: plan_thresholds(ctx, plan.id).await?, inherited_from_plan: false })
            }
            (None, Some(s)) => {
                let sub = org_subscription(ctx, org, &s).await?;
                subscription_thresholds(ctx, sub.id, sub.plan_id).await
            }
            _ => Err(async_graphql::Error::new("exactly_one_of_plan_id_or_subscription_id")),
        }
    }
}

/// Usage-threshold mutation root (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct ThresholdMutation;

#[Object]
impl ThresholdMutation {
    /// Replace a plan's usage thresholds (inherited by its subscriptions that
    /// have no override). An empty list removes progressive billing.
    async fn update_plan_usage_thresholds(
        &self,
        ctx: &Context<'_>,
        plan_id: ID,
        usage_thresholds: Vec<UsageThresholdInput>,
    ) -> Result<UsageThresholdSet> {
        let org = require_org(ctx)?.id;
        let plan = org_plan(ctx, org, &plan_id).await?;
        let set = replace_plan_thresholds(ctx, org, plan.id, &usage_thresholds).await?;
        Ok(UsageThresholdSet { usage_thresholds: set, inherited_from_plan: false })
    }

    /// Override a subscription's usage thresholds. An empty list removes the
    /// override (the plan's thresholds apply again).
    async fn update_subscription_usage_thresholds(
        &self,
        ctx: &Context<'_>,
        subscription_id: ID,
        usage_thresholds: Vec<UsageThresholdInput>,
    ) -> Result<UsageThresholdSet> {
        let org = require_org(ctx)?.id;
        let sub = org_subscription(ctx, org, &subscription_id).await?;
        let parsed = parse_threshold_inputs(&usage_thresholds)?;
        db(ctx)
            .replace_usage_thresholds(org, ThresholdOwner::Subscription(sub.id), &parsed)
            .await?;
        subscription_thresholds(ctx, sub.id, sub.plan_id).await
    }
}

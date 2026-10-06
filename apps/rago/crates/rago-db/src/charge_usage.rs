//! Loading one usage charge for billing: its metric, live filters, the
//! period's events, the events before the period when a prorated recurring
//! charge carries them in, and the aggregation period.
//!
//! Every path that prices usage (period billing, invoice preview, progressive
//! billing, pay-in-advance, current usage) loads through here, so they all see
//! the same filters and the same carried-in value.

use chrono::{NaiveDate, NaiveDateTime};
use rago_billing::aggregation::AggregationPeriod;
use rago_billing::charge_fees::{needs_persisted_events, ChargeUsage, FilterSpec};
use rago_billing::generate::ChargeInput;
use rago_core::models::{BillableMetric, Charge, Event};
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;

/// One usage charge with everything billing reads.
#[derive(Debug, Clone)]
pub struct LoadedCharge {
    pub metric: BillableMetric,
    pub charge: Charge,
    pub filters: Vec<FilterSpec>,
    pub events: Vec<Event>,
    pub persisted_events: Vec<Event>,
    pub period: AggregationPeriod,
}

impl LoadedCharge {
    /// The invoice builder's input.
    pub fn input(&self) -> ChargeInput<'_> {
        ChargeInput {
            metric: &self.metric,
            charge: &self.charge,
            filters: &self.filters,
            events: &self.events,
            persisted_events: &self.persisted_events,
            period: Some(self.period),
        }
    }

    /// The fee engine's input.
    pub fn usage(&self) -> ChargeUsage<'_> {
        ChargeUsage {
            metric: &self.metric,
            charge: &self.charge,
            filters: &self.filters,
            events: &self.events,
            persisted_events: &self.persisted_events,
            period: Some(&self.period),
        }
    }

    /// The same charge over other events (pay-in-advance prices the period
    /// with and without the new event).
    pub fn with_events(&self, events: Vec<Event>) -> Self {
        Self { events, ..self.clone() }
    }
}

impl Db {
    /// Load `charge` for the subscription `(organization_id, external_id)`:
    /// events in `[from, events_to]`, aggregated over `[from, to]`. `None`
    /// when the charge has no billable metric.
    pub async fn load_charge_usage(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        charge: &Charge,
        from: NaiveDateTime,
        to: NaiveDateTime,
        events_to: NaiveDateTime,
    ) -> Result<Option<LoadedCharge>> {
        let Some(metric_id) = charge.billable_metric_id else {
            return Ok(None);
        };
        let metric = self.find_billable_metric(metric_id).await?;
        let events = self
            .events_for_external_subscription(organization_id, external_subscription_id, &metric.code, from, events_to)
            .await?;
        Ok(Some(
            self.complete_charge_usage(
                organization_id,
                external_subscription_id,
                metric,
                charge,
                events,
                from,
                to,
            )
            .await?,
        ))
    }

    /// The rest of a [`LoadedCharge`] for callers that read the period's
    /// events their own way: filters, the carried-in events and the period
    /// (with the weighted-sum carry-in).
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_charge_usage(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        metric: BillableMetric,
        charge: &Charge,
        events: Vec<Event>,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Result<LoadedCharge> {
        let filters = self.charge_filter_specs(charge.id).await?;
        let persisted_events = if needs_persisted_events(&metric, charge) {
            self.events_before(organization_id, external_subscription_id, &metric.code, from)
                .await?
        } else {
            Vec::new()
        };
        let carry_in = self
            .weighted_sum_carry_in(organization_id, external_subscription_id, &metric, from)
            .await?;
        let period = AggregationPeriod::new(from, to).with_initial_value(carry_in);
        Ok(LoadedCharge {
            metric,
            charge: charge.clone(),
            filters,
            events,
            persisted_events,
            period,
        })
    }

    /// Every live event of the subscription's metric strictly before `before`,
    /// oldest first: the value a prorated recurring charge carries into the
    /// period.
    pub async fn events_before(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        code: &str,
        before: NaiveDateTime,
    ) -> Result<Vec<Event>> {
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default();
        let rows = sqlx::query(
            "SELECT * FROM events \
             WHERE external_subscription_id = $1 AND organization_id = $2 AND code = $3 \
             AND timestamp >= $4 AND timestamp < $5 AND deleted_at IS NULL \
             ORDER BY timestamp ASC",
        )
        .bind(external_subscription_id)
        .bind(organization_id)
        .bind(code)
        .bind(epoch)
        .bind(before)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::event).collect()
    }

    /// A plan's live fixed charges as billing reads them. A row whose charge
    /// model cannot be read is an error, never a silently unbilled charge.
    pub async fn fixed_charge_specs(&self, plan_id: Uuid) -> Result<Vec<rago_billing::fixed_charges::FixedChargeSpec>> {
        self.fixed_charges_for_plan(plan_id)
            .await?
            .iter()
            .map(|r| {
                r.spec().ok_or_else(|| {
                    crate::error::DbError::Enum(format!("fixed charge {} has charge_model {}", r.id, r.charge_model))
                })
            })
            .collect()
    }
}

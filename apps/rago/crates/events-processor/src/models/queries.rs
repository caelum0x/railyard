//! sqlx ports of the gorm queries defined across the Go `models` package:
//! `(*ApiStore).FetchSubscription`, `FetchBillableMetric` and `FetchFlatFilters`.
//!
//! Runtime-checked sqlx queries (no compile-time `DATABASE_URL` needed). The
//! streaming `GetAll*` snapshot loaders used by the in-memory cache live in
//! `models::snapshots` (feature `memcache`).

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::utils::result::Result;
use crate::utils::string::StringArray;
use crate::utils::time::NullTime;

use super::billable_metrics::{AggregationType, BillableMetric};
use super::flat_filters::{FlatFilter, FlatFilterValues};
use super::stores::ApiStore;
use super::subscriptions::Subscription;

fn null_time(t: Option<DateTime<Utc>>) -> NullTime {
    NullTime::from_option(t)
}

impl ApiStore {
    /// `(*ApiStore).FetchSubscription`.
    pub async fn fetch_subscription(
        &self,
        organization_id: &str,
        external_id: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<Subscription> {
        const SQL: &str = r#"
            SELECT id, organization_id, external_id, plan_id,
                   created_at, updated_at, started_at, terminated_at
            FROM subscriptions
            WHERE subscriptions.organization_id = $1
              AND subscriptions.external_id = $2
              AND date_trunc('millisecond', subscriptions.started_at::timestamp) <= $3::timestamp
              AND (subscriptions.terminated_at IS NULL
                   OR date_trunc('millisecond', subscriptions.terminated_at::timestamp) >= $3)
            ORDER BY terminated_at DESC NULLS FIRST, started_at DESC
            LIMIT 1
        "#;

        let row = match sqlx::query(SQL)
            .bind(organization_id)
            .bind(external_id)
            .bind(timestamp)
            .fetch_optional(self.db.pool())
            .await
        {
            Ok(r) => r,
            Err(e) => return failed_subscription(e.into()),
        };

        match row {
            None => failed_subscription(crate::utils::RagoError::RecordNotFound),
            Some(row) => Result::success(Subscription {
                id: row.get("id"),
                organization_id: row.get("organization_id"),
                external_id: row.get("external_id"),
                plan_id: row.get("plan_id"),
                created_at: null_time(row.get("created_at")),
                updated_at: null_time(row.get("updated_at")),
                started_at: null_time(row.get("started_at")),
                terminated_at: null_time(row.get("terminated_at")),
            }),
        }
    }

    /// `(*ApiStore).FetchBillableMetric`.
    pub async fn fetch_billable_metric(
        &self,
        organization_id: &str,
        code: &str,
    ) -> Result<BillableMetric> {
        const SQL: &str = r#"
            SELECT id, organization_id, code, aggregation_type, recurring,
                   field_name, expression, created_at, updated_at, deleted_at
            FROM billable_metrics
            WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL
            ORDER BY id
            LIMIT 1
        "#;

        let row = match sqlx::query(SQL)
            .bind(organization_id)
            .bind(code)
            .fetch_optional(self.db.pool())
            .await
        {
            Ok(r) => r,
            Err(e) => return failed_billable_metric(e.into()),
        };

        match row {
            None => failed_billable_metric(crate::utils::RagoError::RecordNotFound),
            Some(row) => {
                let agg: i32 = row.get("aggregation_type");
                Result::success(BillableMetric {
                    id: row.get("id"),
                    organization_id: row.get("organization_id"),
                    code: row.get("code"),
                    aggregation_type: AggregationType(agg),
                    recurring: row.get("recurring"),
                    field_name: row.try_get("field_name").unwrap_or_default(),
                    expression: row.try_get("expression").unwrap_or_default(),
                    created_at: null_time(row.get("created_at")),
                    updated_at: null_time(row.get("updated_at")),
                    deleted_at: null_time(row.try_get("deleted_at").unwrap_or(None)),
                })
            }
        }
    }

    /// `(*ApiStore).FetchFlatFilters`.
    pub async fn fetch_flat_filters(
        &self,
        organization_id: &str,
        plan_id: &str,
        billable_metric_code: &str,
    ) -> Result<Vec<FlatFilter>> {
        const SQL: &str = r#"
            SELECT organization_id, billable_metric_code, plan_id, charge_id,
                   charge_updated_at, charge_filter_id, charge_filter_updated_at,
                   filters, pricing_group_keys, pay_in_advance, accepts_target_wallet
            FROM flat_filters
            WHERE organization_id = $1 AND plan_id = $2 AND billable_metric_code = $3
        "#;

        let rows = match sqlx::query(SQL)
            .bind(organization_id)
            .bind(plan_id)
            .bind(billable_metric_code)
            .fetch_all(self.db.pool())
            .await
        {
            Ok(r) => r,
            Err(e) => return Result::failed(crate::utils::RagoError::from(e)),
        };

        let mut filters = Vec::with_capacity(rows.len());
        for row in rows {
            let filters_json: Option<serde_json::Value> = row.try_get("filters").unwrap_or(None);
            let pgk_json: Option<serde_json::Value> =
                row.try_get("pricing_group_keys").unwrap_or(None);

            let flat_values: Option<FlatFilterValues> =
                filters_json.and_then(|v| serde_json::from_value::<FlatFilterValues>(v).ok());
            let pricing_group_keys: Vec<String> = pgk_json
                .and_then(|v| serde_json::from_value::<StringArray>(v).ok())
                .map(StringArray::into_vec)
                .unwrap_or_default();

            let charge_updated_at: Option<DateTime<Utc>> =
                row.try_get("charge_updated_at").unwrap_or(None);

            filters.push(FlatFilter {
                organization_id: row.get("organization_id"),
                billable_metric_code: row.get("billable_metric_code"),
                plan_id: row.get("plan_id"),
                charge_id: row.get("charge_id"),
                charge_updated_at: charge_updated_at.unwrap_or(DateTime::<Utc>::MIN_UTC),
                charge_filter_id: row.try_get("charge_filter_id").unwrap_or(None),
                charge_filter_updated_at: row.try_get("charge_filter_updated_at").unwrap_or(None),
                filters: flat_values,
                pricing_group_keys,
                pay_in_advance: row.try_get("pay_in_advance").unwrap_or(false),
                accepts_target_wallet: row.try_get("accepts_target_wallet").unwrap_or(false),
            });
        }

        Result::success(filters)
    }
}

/// `failedSubscriptionResult`.
fn failed_subscription(err: crate::utils::RagoError) -> Result<Subscription> {
    let mut result = Result::<Subscription>::failed(err.clone());
    if err.is_record_not_found() {
        result = result.non_capturable().non_retryable();
    }
    result
}

/// `failedBillabmeMetricResult`.
fn failed_billable_metric(err: crate::utils::RagoError) -> Result<BillableMetric> {
    let mut result = Result::<BillableMetric>::failed(err.clone());
    if err.is_record_not_found() {
        result = result.non_capturable().non_retryable();
    }
    result
}

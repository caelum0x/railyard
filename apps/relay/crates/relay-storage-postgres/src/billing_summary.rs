//! Postgres aggregation for Relay's billing dashboard.
//!
//! Rolls up the durable `relay_cost_events` outbox and
//! `relay_settlement_evidence` tables into a [`BillingSummary`] for a single
//! authenticated principal. Every query is principal-scoped so the summary can
//! never surface another tenant's spend.

use relay_contract::{
    BillingSummary, KeySpend, KeySpendPoint, Microdollars, ModelSpend, SettlementDetail,
    SettlementRecord, SpendTotals, WalletSummary,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Default number of recent settlement receipts returned by the summary.
pub const DEFAULT_SETTLEMENT_HISTORY_LIMIT: i64 = 50;
/// Upper bound on the settlement-history window so the query stays cheap.
pub const MAX_SETTLEMENT_HISTORY_LIMIT: i64 = 500;

#[derive(Clone)]
pub struct PostgresBillingSummaryStore {
    pool: PgPool,
}

impl PostgresBillingSummaryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Aggregate the principal's cost events and settlement evidence.
    ///
    /// `settlement_limit` is clamped to `[1, MAX_SETTLEMENT_HISTORY_LIMIT]`.
    pub async fn summary(
        &self,
        owner_principal: &str,
        settlement_limit: i64,
    ) -> Result<BillingSummary, sqlx::Error> {
        let settlement_limit = settlement_limit.clamp(1, MAX_SETTLEMENT_HISTORY_LIMIT);

        // Cost-event totals. The immutable event body is JSONB, so exact
        // micro-USD money fields are extracted and summed as bigints.
        let totals_row = sqlx::query(
            "SELECT \
             COUNT(*)::bigint AS event_count, \
             COALESCE(SUM((event->>'charged_micros')::bigint),0)::bigint AS charged, \
             COALESCE(SUM((event->>'authorized_micros')::bigint),0)::bigint AS authorized, \
             COALESCE(SUM((event->'usage'->>'input_tokens')::bigint),0)::bigint AS input_tokens, \
             COALESCE(SUM((event->'usage'->>'output_tokens')::bigint),0)::bigint AS output_tokens, \
             COALESCE(SUM((event->'usage'->>'total_tokens')::bigint),0)::bigint AS total_tokens \
             FROM relay_cost_events WHERE owner_principal=$1",
        )
        .bind(owner_principal)
        .fetch_one(&self.pool)
        .await?;

        let cost_event_count: i64 = totals_row.try_get("event_count")?;
        let totals = SpendTotals {
            charged_micros: Microdollars::new(totals_row.try_get("charged")?),
            authorized_micros: Microdollars::new(totals_row.try_get("authorized")?),
            input_tokens: totals_row.try_get("input_tokens")?,
            output_tokens: totals_row.try_get("output_tokens")?,
            total_tokens: totals_row.try_get("total_tokens")?,
        };

        // Settlement-evidence rollup: the signed receipt carries the exact
        // captured amount in `amount_micros`.
        let settlement_row = sqlx::query(
            "SELECT \
             COUNT(*)::bigint AS settlement_count, \
             COALESCE(SUM((receipt->>'amount_micros')::bigint),0)::bigint AS settled_total, \
             MAX(settled_at_ms) AS last_settled_at_ms \
             FROM relay_settlement_evidence WHERE principal=$1",
        )
        .bind(owner_principal)
        .fetch_one(&self.pool)
        .await?;

        let wallet = WalletSummary {
            total_charged_micros: totals.charged_micros,
            total_authorized_micros: totals.authorized_micros,
            total_settled_micros: Microdollars::new(settlement_row.try_get("settled_total")?),
            cost_event_count,
            settlement_count: settlement_row.try_get("settlement_count")?,
            last_settled_at_ms: settlement_row.try_get("last_settled_at_ms")?,
        };

        let by_key = sqlx::query(
            "SELECT owner_principal AS key, \
             COALESCE(SUM((event->>'charged_micros')::bigint),0)::bigint AS charged, \
             COUNT(*)::bigint AS event_count \
             FROM relay_cost_events WHERE owner_principal=$1 \
             GROUP BY owner_principal ORDER BY charged DESC",
        )
        .bind(owner_principal)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(KeySpend {
                key: row.try_get("key")?,
                charged_micros: Microdollars::new(row.try_get("charged")?),
                cost_event_count: row.try_get("event_count")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

        let by_model = sqlx::query(
            "SELECT event->>'model' AS model, \
             COALESCE(SUM((event->>'charged_micros')::bigint),0)::bigint AS charged, \
             COALESCE(SUM((event->'usage'->>'total_tokens')::bigint),0)::bigint AS total_tokens, \
             COUNT(*)::bigint AS event_count \
             FROM relay_cost_events WHERE owner_principal=$1 \
             GROUP BY event->>'model' ORDER BY charged DESC",
        )
        .bind(owner_principal)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(ModelSpend {
                model: row
                    .try_get::<Option<String>, _>("model")?
                    .unwrap_or_default(),
                charged_micros: Microdollars::new(row.try_get("charged")?),
                total_tokens: row.try_get("total_tokens")?,
                cost_event_count: row.try_get("event_count")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

        let recent_settlements = sqlx::query(
            "SELECT request_id, \
             COALESCE((receipt->>'amount_micros')::bigint,0)::bigint AS amount, \
             settled_at_ms, verified_at_ms \
             FROM relay_settlement_evidence WHERE principal=$1 \
             ORDER BY settled_at_ms DESC LIMIT $2",
        )
        .bind(owner_principal)
        .bind(settlement_limit)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(SettlementRecord {
                request_id: row.try_get::<Uuid, _>("request_id")?,
                amount_micros: Microdollars::new(row.try_get::<i64, _>("amount")?),
                settled_at_ms: row.try_get("settled_at_ms")?,
                verified_at_ms: row.try_get("verified_at_ms")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

        Ok(BillingSummary {
            principal: owner_principal.to_string(),
            wallet,
            totals,
            by_key,
            by_model,
            recent_settlements,
        })
    }

    /// Detailed settlement-evidence history for the principal, newest first.
    ///
    /// Each row carries the immutable signed receipt (as a JSON string) so the
    /// dashboard can render the exact proof of payment. `limit` is clamped to
    /// `[1, MAX_SETTLEMENT_HISTORY_LIMIT]`.
    pub async fn settlements(
        &self,
        owner_principal: &str,
        limit: i64,
    ) -> Result<Vec<SettlementDetail>, sqlx::Error> {
        let limit = limit.clamp(1, MAX_SETTLEMENT_HISTORY_LIMIT);
        sqlx::query(
            "SELECT request_id, \
             COALESCE((receipt->>'amount_micros')::bigint,0)::bigint AS amount, \
             settled_at_ms, verified_at_ms, \
             (attestation IS NOT NULL) AS has_attestation, \
             receipt::text AS receipt_json \
             FROM relay_settlement_evidence WHERE principal=$1 \
             ORDER BY settled_at_ms DESC LIMIT $2",
        )
        .bind(owner_principal)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(SettlementDetail {
                request_id: row.try_get::<Uuid, _>("request_id")?,
                amount_micros: Microdollars::new(row.try_get::<i64, _>("amount")?),
                settled_at_ms: row.try_get("settled_at_ms")?,
                verified_at_ms: row.try_get("verified_at_ms")?,
                has_attestation: row.try_get("has_attestation")?,
                receipt_json: row
                    .try_get::<Option<String>, _>("receipt_json")?
                    .unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
    }

    /// Per-API-key spend over time (UTC day buckets) for the principal.
    ///
    /// Buckets are derived from each cost event's `created_at` timestamp and
    /// returned oldest-first so the dashboard can plot a spend trend.
    pub async fn key_spend_timeseries(
        &self,
        owner_principal: &str,
    ) -> Result<Vec<KeySpendPoint>, sqlx::Error> {
        sqlx::query(
            "SELECT owner_principal AS key, \
             (EXTRACT(EPOCH FROM date_trunc('day', created_at)) * 1000)::bigint AS bucket_start_ms, \
             COALESCE(SUM((event->>'charged_micros')::bigint),0)::bigint AS charged, \
             COALESCE(SUM((event->'usage'->>'total_tokens')::bigint),0)::bigint AS total_tokens, \
             COUNT(*)::bigint AS event_count \
             FROM relay_cost_events WHERE owner_principal=$1 \
             GROUP BY owner_principal, date_trunc('day', created_at) \
             ORDER BY bucket_start_ms ASC",
        )
        .bind(owner_principal)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(KeySpendPoint {
                key: row.try_get("key")?,
                bucket_start_ms: row.try_get("bucket_start_ms")?,
                charged_micros: Microdollars::new(row.try_get("charged")?),
                total_tokens: row.try_get("total_tokens")?,
                cost_event_count: row.try_get("event_count")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
    }
}

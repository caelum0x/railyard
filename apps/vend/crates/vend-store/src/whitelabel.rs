//! Postgres implementation of [`WhiteLabelRepo`]: per-model markups, wallet
//! listing, tenant earnings and payout visibility (sqlx runtime API only).

use async_trait::async_trait;
use sqlx::Row;

use vend_core::ids::{AppId, TenantId};
use vend_core::money::ModelMarkup;
use vend_core::whitelabel::{
    EarningsDay, ModelMarkupEntry, PayoutRow, PayoutTotals, WalletSummary, WhiteLabelRepo,
};
use vend_core::VendError;

use crate::VendStore;

fn store_err(error: sqlx::Error) -> VendError {
    VendError::Storage(error.to_string())
}

/// Longest model id accepted (matches the table CHECK).
const MAX_MODEL_LEN: usize = 256;

fn validate_model(model: &str) -> Result<(), VendError> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL_LEN || model.chars().any(char::is_control) {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

fn markup_from_row(row: &sqlx::postgres::PgRow) -> Result<ModelMarkupEntry, VendError> {
    let bps: i32 = row.try_get("markup_bps").map_err(store_err)?;
    Ok(ModelMarkupEntry {
        model: row.try_get("model").map_err(store_err)?,
        markup: ModelMarkup {
            markup_bps: u32::try_from(bps).map_err(|_| VendError::InvalidAmount)?,
            min_fee_micros: row.try_get("min_fee_micros").map_err(store_err)?,
        },
        updated_at_ms: row.try_get("updated_at_ms").map_err(store_err)?,
    })
}

#[async_trait]
impl WhiteLabelRepo for VendStore {
    async fn list_model_markups(&self, app_id: &AppId) -> Result<Vec<ModelMarkupEntry>, VendError> {
        let rows = sqlx::query(
            "SELECT model, markup_bps, min_fee_micros, \
             (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_at_ms \
             FROM app_model_markups WHERE app_id = $1 ORDER BY model",
        )
        .bind(app_id.as_str())
        .fetch_all(self.pool())
        .await
        .map_err(store_err)?;
        rows.iter().map(markup_from_row).collect()
    }

    async fn set_model_markup(
        &self,
        app_id: &AppId,
        model: &str,
        markup: ModelMarkup,
    ) -> Result<ModelMarkupEntry, VendError> {
        validate_model(model)?;
        markup.validate()?;
        let bps = i32::try_from(markup.markup_bps).map_err(|_| VendError::InvalidAmount)?;
        let row = sqlx::query(
            "INSERT INTO app_model_markups (app_id, model, markup_bps, min_fee_micros) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (app_id, model) DO UPDATE \
             SET markup_bps = EXCLUDED.markup_bps, min_fee_micros = EXCLUDED.min_fee_micros, \
                 updated_at = now() \
             RETURNING model, markup_bps, min_fee_micros, \
             (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_at_ms",
        )
        .bind(app_id.as_str())
        .bind(model.trim())
        .bind(bps)
        .bind(markup.min_fee_micros)
        .fetch_one(self.pool())
        .await
        .map_err(store_err)?;
        markup_from_row(&row)
    }

    async fn delete_model_markup(&self, app_id: &AppId, model: &str) -> Result<bool, VendError> {
        let done = sqlx::query("DELETE FROM app_model_markups WHERE app_id = $1 AND model = $2")
            .bind(app_id.as_str())
            .bind(model.trim())
            .execute(self.pool())
            .await
            .map_err(store_err)?;
        Ok(done.rows_affected() > 0)
    }

    async fn list_wallets(
        &self,
        app_id: &AppId,
        utc_day: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WalletSummary>, VendError> {
        let rows = sqlx::query(
            "SELECT e.id, e.external_ref, e.mint_account_id, \
             (EXTRACT(EPOCH FROM e.created_at) * 1000)::BIGINT AS created_at_ms, \
             COALESCE((SELECT SUM(u.charge_micros) FROM metered_usage u \
                       WHERE u.end_user_id = e.id AND u.utc_day = $2 \
                       AND u.outcome <> 'released'), 0)::BIGINT AS spent_today \
             FROM end_users e WHERE e.app_id = $1 \
             ORDER BY e.created_at DESC, e.id DESC LIMIT $3 OFFSET $4",
        )
        .bind(app_id.as_str())
        .bind(utc_day)
        .bind(limit.clamp(1, 500))
        .bind(offset.max(0))
        .fetch_all(self.pool())
        .await
        .map_err(store_err)?;
        rows.iter()
            .map(|row| {
                Ok(WalletSummary {
                    end_user_id: row.try_get("id").map_err(store_err)?,
                    external_ref: row.try_get("external_ref").map_err(store_err)?,
                    mint_account_id: row.try_get("mint_account_id").map_err(store_err)?,
                    spent_today_micros: row.try_get("spent_today").map_err(store_err)?,
                    created_at_ms: row.try_get("created_at_ms").map_err(store_err)?,
                })
            })
            .collect()
    }

    async fn tenant_earnings(
        &self,
        tenant_id: &TenantId,
        from: &str,
        to: &str,
    ) -> Result<Vec<EarningsDay>, VendError> {
        let rows = sqlx::query(
            "SELECT utc_day, COUNT(*)::BIGINT AS captured_requests, \
             COALESCE(SUM(charge_micros), 0)::BIGINT AS charge, \
             COALESCE(SUM(provider_cost_micros), 0)::BIGINT AS provider_cost, \
             COALESCE(SUM(platform_fee_micros), 0)::BIGINT AS platform_fee, \
             COALESCE(SUM(tenant_net_micros), 0)::BIGINT AS tenant_net \
             FROM metered_usage \
             WHERE tenant_id = $1 AND outcome = 'captured' AND utc_day BETWEEN $2 AND $3 \
             GROUP BY utc_day ORDER BY utc_day",
        )
        .bind(tenant_id.as_str())
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await
        .map_err(store_err)?;
        rows.iter()
            .map(|row| {
                Ok(EarningsDay {
                    utc_day: row.try_get("utc_day").map_err(store_err)?,
                    captured_requests: row.try_get("captured_requests").map_err(store_err)?,
                    charge_micros: row.try_get("charge").map_err(store_err)?,
                    provider_cost_micros: row.try_get("provider_cost").map_err(store_err)?,
                    platform_fee_micros: row.try_get("platform_fee").map_err(store_err)?,
                    tenant_net_micros: row.try_get("tenant_net").map_err(store_err)?,
                })
            })
            .collect()
    }

    async fn list_payouts(
        &self,
        tenant_id: Option<&TenantId>,
        pending_only: bool,
        limit: i64,
    ) -> Result<Vec<PayoutRow>, VendError> {
        let rows = sqlx::query(
            "SELECT p.charge_id, u.tenant_id, u.app_id, p.captured_charge_micros, \
             p.provider_cost_micros, p.tenant_net_micros, p.platform_fee_micros, \
             p.provider_transferred, p.tenant_transferred, p.platform_transferred, \
             p.state, p.last_error, p.tenant_account_id, \
             (EXTRACT(EPOCH FROM p.created_at) * 1000)::BIGINT AS created_at_ms \
             FROM payout_executions p JOIN metered_usage u ON u.charge_id = p.charge_id \
             WHERE ($1::TEXT IS NULL OR u.tenant_id = $1) \
             AND (NOT $2 OR p.state = 'pending') \
             ORDER BY p.created_at DESC LIMIT $3",
        )
        .bind(tenant_id.map(TenantId::as_str))
        .bind(pending_only)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.pool())
        .await
        .map_err(store_err)?;
        rows.iter()
            .map(|row| {
                Ok(PayoutRow {
                    charge_id: row.try_get("charge_id").map_err(store_err)?,
                    tenant_id: row.try_get("tenant_id").map_err(store_err)?,
                    app_id: row.try_get("app_id").map_err(store_err)?,
                    captured_charge_micros: row
                        .try_get("captured_charge_micros")
                        .map_err(store_err)?,
                    provider_cost_micros: row.try_get("provider_cost_micros").map_err(store_err)?,
                    tenant_net_micros: row.try_get("tenant_net_micros").map_err(store_err)?,
                    platform_fee_micros: row.try_get("platform_fee_micros").map_err(store_err)?,
                    provider_transferred: row.try_get("provider_transferred").map_err(store_err)?,
                    tenant_transferred: row.try_get("tenant_transferred").map_err(store_err)?,
                    platform_transferred: row.try_get("platform_transferred").map_err(store_err)?,
                    state: row.try_get("state").map_err(store_err)?,
                    last_error: row.try_get("last_error").map_err(store_err)?,
                    tenant_account_id: row.try_get("tenant_account_id").map_err(store_err)?,
                    created_at_ms: row.try_get("created_at_ms").map_err(store_err)?,
                })
            })
            .collect()
    }

    async fn payout_totals(&self, tenant_id: Option<&TenantId>) -> Result<PayoutTotals, VendError> {
        let row = sqlx::query(
            "SELECT \
             COUNT(*) FILTER (WHERE p.state = 'complete')::BIGINT AS complete_count, \
             COUNT(*) FILTER (WHERE p.state = 'pending')::BIGINT AS pending_count, \
             COALESCE(SUM(p.tenant_net_micros) FILTER (WHERE p.tenant_transferred), 0)::BIGINT AS tenant_paid, \
             COALESCE(SUM(p.tenant_net_micros) FILTER (WHERE p.state = 'pending' AND NOT p.tenant_transferred), 0)::BIGINT AS tenant_pending, \
             COALESCE(SUM(p.platform_fee_micros) FILTER (WHERE p.platform_transferred), 0)::BIGINT AS platform_paid \
             FROM payout_executions p JOIN metered_usage u ON u.charge_id = p.charge_id \
             WHERE ($1::TEXT IS NULL OR u.tenant_id = $1)",
        )
        .bind(tenant_id.map(TenantId::as_str))
        .fetch_one(self.pool())
        .await
        .map_err(store_err)?;
        Ok(PayoutTotals {
            complete_count: row.try_get("complete_count").map_err(store_err)?,
            pending_count: row.try_get("pending_count").map_err(store_err)?,
            tenant_paid_micros: row.try_get("tenant_paid").map_err(store_err)?,
            tenant_pending_micros: row.try_get("tenant_pending").map_err(store_err)?,
            platform_paid_micros: row.try_get("platform_paid").map_err(store_err)?,
        })
    }
}

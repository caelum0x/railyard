use async_trait::async_trait;
use sqlx::{PgPool, Row};

use vend_core::payout::{PayoutExecution, PayoutLeg, PayoutRepo, PayoutState};
use vend_core::{ChargeId, VendError};

use crate::VendStore;

fn store_err(error: sqlx::Error) -> VendError {
    VendError::Storage(error.to_string())
}

fn state(value: &str) -> Result<PayoutState, VendError> {
    match value {
        "pending" => Ok(PayoutState::Pending),
        "complete" => Ok(PayoutState::Complete),
        "cancelled" => Ok(PayoutState::Cancelled),
        _ => Err(VendError::Storage("invalid payout state".into())),
    }
}

async fn load(pool: &PgPool, charge_id: &ChargeId) -> Result<Option<PayoutExecution>, VendError> {
    let row = sqlx::query(
        "SELECT charge_id, idem_key, clearing_account_id, provider_account_id, \
         tenant_account_id, platform_account_id, provider_cost_micros, \
         tenant_net_micros, platform_fee_micros, captured_charge_micros, \
         provider_transferred, tenant_transferred, platform_transferred, state, last_error \
         FROM payout_executions WHERE charge_id = $1",
    )
    .bind(charge_id.as_str())
    .fetch_optional(pool)
    .await
    .map_err(store_err)?;
    row.map(|row| {
        Ok(PayoutExecution {
            charge_id: ChargeId::parse(&row.try_get::<String, _>("charge_id").map_err(store_err)?)?,
            idem_key: row.try_get("idem_key").map_err(store_err)?,
            clearing_account_id: row.try_get("clearing_account_id").map_err(store_err)?,
            provider_account_id: row.try_get("provider_account_id").map_err(store_err)?,
            tenant_account_id: row.try_get("tenant_account_id").map_err(store_err)?,
            platform_account_id: row.try_get("platform_account_id").map_err(store_err)?,
            provider_cost_micros: row.try_get("provider_cost_micros").map_err(store_err)?,
            tenant_net_micros: row.try_get("tenant_net_micros").map_err(store_err)?,
            platform_fee_micros: row.try_get("platform_fee_micros").map_err(store_err)?,
            captured_charge_micros: row.try_get("captured_charge_micros").map_err(store_err)?,
            provider_transferred: row.try_get("provider_transferred").map_err(store_err)?,
            tenant_transferred: row.try_get("tenant_transferred").map_err(store_err)?,
            platform_transferred: row.try_get("platform_transferred").map_err(store_err)?,
            state: state(&row.try_get::<String, _>("state").map_err(store_err)?)?,
            last_error: row.try_get("last_error").map_err(store_err)?,
        })
    })
    .transpose()
}

fn immutable_plan_matches(left: &PayoutExecution, right: &PayoutExecution) -> bool {
    left.charge_id == right.charge_id
        && left.idem_key == right.idem_key
        && left.clearing_account_id == right.clearing_account_id
        && left.provider_account_id == right.provider_account_id
        && left.tenant_account_id == right.tenant_account_id
        && left.platform_account_id == right.platform_account_id
        && left.provider_cost_micros == right.provider_cost_micros
        && left.tenant_net_micros == right.tenant_net_micros
        && left.platform_fee_micros == right.platform_fee_micros
        && left.captured_charge_micros == right.captured_charge_micros
}

#[async_trait]
impl PayoutRepo for VendStore {
    async fn prepare_payout(&self, payout: &PayoutExecution) -> Result<(), VendError> {
        sqlx::query(
            "INSERT INTO payout_executions (charge_id, idem_key, clearing_account_id, \
             provider_account_id, tenant_account_id, platform_account_id, \
             provider_cost_micros, tenant_net_micros, platform_fee_micros, \
             captured_charge_micros, provider_transferred, tenant_transferred, \
             platform_transferred) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) \
             ON CONFLICT (charge_id) DO NOTHING",
        )
        .bind(payout.charge_id.as_str())
        .bind(&payout.idem_key)
        .bind(&payout.clearing_account_id)
        .bind(&payout.provider_account_id)
        .bind(&payout.tenant_account_id)
        .bind(&payout.platform_account_id)
        .bind(payout.provider_cost_micros)
        .bind(payout.tenant_net_micros)
        .bind(payout.platform_fee_micros)
        .bind(payout.captured_charge_micros)
        .bind(payout.provider_transferred)
        .bind(payout.tenant_transferred)
        .bind(payout.platform_transferred)
        .execute(self.pool())
        .await
        .map_err(store_err)?;

        let stored = load(self.pool(), &payout.charge_id)
            .await?
            .ok_or_else(|| VendError::Storage("payout plan disappeared after insert".into()))?;
        if immutable_plan_matches(&stored, payout) {
            Ok(())
        } else {
            Err(VendError::IdempotencyConflict)
        }
    }

    async fn load_payout(
        &self,
        charge_id: &ChargeId,
    ) -> Result<Option<PayoutExecution>, VendError> {
        load(self.pool(), charge_id).await
    }

    async fn pending_payouts_before(
        &self,
        older_than_unix_seconds: i64,
        limit: u32,
    ) -> Result<Vec<PayoutExecution>, VendError> {
        let rows = sqlx::query(
            "SELECT charge_id FROM payout_executions \
             WHERE state = 'pending' AND updated_at < to_timestamp($1) \
             ORDER BY updated_at, charge_id LIMIT $2",
        )
        .bind(older_than_unix_seconds as f64)
        .bind(i64::from(limit.min(1_000)))
        .fetch_all(self.pool())
        .await
        .map_err(store_err)?;
        let mut payouts = Vec::with_capacity(rows.len());
        for row in rows {
            let raw: String = row.try_get("charge_id").map_err(store_err)?;
            let charge_id = ChargeId::parse(&raw)?;
            if let Some(payout) = load(self.pool(), &charge_id).await? {
                payouts.push(payout);
            }
        }
        Ok(payouts)
    }

    async fn mark_payout_leg(&self, charge_id: &ChargeId, leg: PayoutLeg) -> Result<(), VendError> {
        let column = match leg {
            PayoutLeg::Provider => "provider_transferred",
            PayoutLeg::Tenant => "tenant_transferred",
            PayoutLeg::Platform => "platform_transferred",
        };
        let query = format!(
            "UPDATE payout_executions SET {column} = true, last_error = NULL, updated_at = now() \
             WHERE charge_id = $1 AND state = 'pending'"
        );
        sqlx::query(&query)
            .bind(charge_id.as_str())
            .execute(self.pool())
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn mark_payout_complete(&self, charge_id: &ChargeId) -> Result<(), VendError> {
        let result = sqlx::query(
            "UPDATE payout_executions SET state = 'complete', last_error = NULL, updated_at = now() \
             WHERE charge_id = $1 AND provider_transferred AND tenant_transferred \
             AND platform_transferred AND state = 'pending'",
        )
        .bind(charge_id.as_str())
        .execute(self.pool())
        .await
        .map_err(store_err)?;
        if result.rows_affected() == 1
            || load(self.pool(), charge_id)
                .await?
                .is_some_and(|payout| payout.state == PayoutState::Complete)
        {
            Ok(())
        } else {
            Err(VendError::Storage(
                "payout cannot complete before every leg is observed".into(),
            ))
        }
    }

    async fn record_payout_error(
        &self,
        charge_id: &ChargeId,
        message: &str,
    ) -> Result<(), VendError> {
        sqlx::query(
            "UPDATE payout_executions SET last_error = LEFT($2, 2000), updated_at = now() \
             WHERE charge_id = $1 AND state = 'pending'",
        )
        .bind(charge_id.as_str())
        .bind(message)
        .execute(self.pool())
        .await
        .map_err(store_err)?;
        Ok(())
    }
}

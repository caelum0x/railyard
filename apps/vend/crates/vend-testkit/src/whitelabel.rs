//! In-memory [`WhiteLabelRepo`] for [`InMemoryStore`]: markups are stored per
//! app, wallets/earnings/payouts are derived from the seeded end users, usage
//! and payout rows exactly as the Postgres queries derive them.

use async_trait::async_trait;

use vend_core::error::VendError;
use vend_core::ids::{AppId, TenantId};
use vend_core::money::ModelMarkup;
use vend_core::payout::PayoutState;
use vend_core::ports::UsageOutcome;
use vend_core::whitelabel::{
    EarningsDay, ModelMarkupEntry, PayoutRow, PayoutTotals, WalletSummary, WhiteLabelRepo,
};

use crate::store::InMemoryStore;

fn state_str(state: PayoutState) -> &'static str {
    match state {
        PayoutState::Pending => "pending",
        PayoutState::Complete => "complete",
        PayoutState::Cancelled => "cancelled",
    }
}

#[async_trait]
impl WhiteLabelRepo for InMemoryStore {
    async fn list_model_markups(&self, app_id: &AppId) -> Result<Vec<ModelMarkupEntry>, VendError> {
        Ok(self.with_state(|s| {
            s.apps
                .get(app_id.as_str())
                .map(|app| {
                    app.model_markups
                        .iter()
                        .map(|(model, markup)| ModelMarkupEntry {
                            model: model.clone(),
                            markup: *markup,
                            updated_at_ms: 0,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }))
    }

    async fn set_model_markup(
        &self,
        app_id: &AppId,
        model: &str,
        markup: ModelMarkup,
    ) -> Result<ModelMarkupEntry, VendError> {
        markup.validate()?;
        if model.trim().is_empty() || model.len() > 256 {
            return Err(VendError::InvalidAmount);
        }
        self.with_state(|s| {
            let app = s.apps.get_mut(app_id.as_str()).ok_or(VendError::NotFound)?;
            app.model_markups.insert(model.trim().to_string(), markup);
            Ok(ModelMarkupEntry {
                model: model.trim().to_string(),
                markup,
                updated_at_ms: 0,
            })
        })
    }

    async fn delete_model_markup(&self, app_id: &AppId, model: &str) -> Result<bool, VendError> {
        Ok(self.with_state(|s| {
            s.apps
                .get_mut(app_id.as_str())
                .is_some_and(|app| app.model_markups.remove(model.trim()).is_some())
        }))
    }

    async fn list_wallets(
        &self,
        app_id: &AppId,
        utc_day: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WalletSummary>, VendError> {
        Ok(self.with_state(|s| {
            let mut wallets: Vec<WalletSummary> = s
                .end_users
                .values()
                .filter(|eu| eu.app_id == *app_id)
                .map(|eu| WalletSummary {
                    end_user_id: eu.id.as_str().to_string(),
                    external_ref: eu.external_ref.clone(),
                    mint_account_id: eu.mint_account_id.clone(),
                    spent_today_micros: s
                        .usage
                        .values()
                        .filter(|u| {
                            u.end_user_id.as_ref() == Some(&eu.id)
                                && u.utc_day == utc_day
                                && u.outcome != UsageOutcome::Released
                        })
                        .map(|u| u.charge_micros)
                        .sum(),
                    created_at_ms: 0,
                })
                .collect();
            wallets.sort_by(|a, b| b.end_user_id.cmp(&a.end_user_id));
            wallets
                .into_iter()
                .skip(offset.max(0) as usize)
                .take(limit.clamp(1, 500) as usize)
                .collect()
        }))
    }

    async fn tenant_earnings(
        &self,
        tenant_id: &TenantId,
        from: &str,
        to: &str,
    ) -> Result<Vec<EarningsDay>, VendError> {
        Ok(self.with_state(|s| {
            let mut days: std::collections::BTreeMap<String, EarningsDay> = Default::default();
            for u in s.usage.values().filter(|u| {
                u.tenant_id == *tenant_id
                    && u.outcome == UsageOutcome::Captured
                    && u.utc_day.as_str() >= from
                    && u.utc_day.as_str() <= to
            }) {
                let day = days
                    .entry(u.utc_day.clone())
                    .or_insert_with(|| EarningsDay {
                        utc_day: u.utc_day.clone(),
                        ..EarningsDay::default()
                    });
                day.captured_requests += 1;
                day.charge_micros += u.charge_micros;
                day.provider_cost_micros += u.provider_cost_micros;
                day.platform_fee_micros += u.platform_fee_micros;
                day.tenant_net_micros += u.tenant_net_micros;
            }
            days.into_values().collect()
        }))
    }

    async fn list_payouts(
        &self,
        tenant_id: Option<&TenantId>,
        pending_only: bool,
        limit: i64,
    ) -> Result<Vec<PayoutRow>, VendError> {
        Ok(self.with_state(|s| {
            s.payouts
                .values()
                .filter_map(|p| {
                    let usage = s.usage.get(p.charge_id.as_str())?;
                    if tenant_id.is_some_and(|t| *t != usage.tenant_id)
                        || (pending_only && p.state != PayoutState::Pending)
                    {
                        return None;
                    }
                    Some(PayoutRow {
                        charge_id: p.charge_id.as_str().to_string(),
                        tenant_id: usage.tenant_id.as_str().to_string(),
                        app_id: usage.app_id.as_str().to_string(),
                        captured_charge_micros: p.captured_charge_micros,
                        provider_cost_micros: p.provider_cost_micros,
                        tenant_net_micros: p.tenant_net_micros,
                        platform_fee_micros: p.platform_fee_micros,
                        provider_transferred: p.provider_transferred,
                        tenant_transferred: p.tenant_transferred,
                        platform_transferred: p.platform_transferred,
                        state: state_str(p.state).to_string(),
                        last_error: p.last_error.clone(),
                        tenant_account_id: p.tenant_account_id.clone(),
                        created_at_ms: 0,
                    })
                })
                .take(limit.clamp(1, 500) as usize)
                .collect()
        }))
    }

    async fn payout_totals(&self, tenant_id: Option<&TenantId>) -> Result<PayoutTotals, VendError> {
        let rows = self.list_payouts(tenant_id, false, 500).await?;
        Ok(rows.iter().fold(PayoutTotals::default(), |mut t, p| {
            match p.state.as_str() {
                "complete" => t.complete_count += 1,
                "pending" => t.pending_count += 1,
                _ => {}
            }
            if p.tenant_transferred {
                t.tenant_paid_micros += p.tenant_net_micros;
            } else if p.state == "pending" {
                t.tenant_pending_micros += p.tenant_net_micros;
            }
            if p.platform_transferred {
                t.platform_paid_micros += p.platform_fee_micros;
            }
            t
        }))
    }
}

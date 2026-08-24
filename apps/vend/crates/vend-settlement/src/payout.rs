//! Restart-safe distribution of a captured customer charge.
//!
//! Sx0 is deliberately bounded to `tenant_reserve`: an app reserve funds the
//! hold, capture credits a configured Vend clearing account, then the exact
//! captured amount is conserved across provider cost, tenant net, and platform
//! fee transfers. The immutable plan is persisted before capture. Each leg has
//! its own stable Mint idempotency key, so a worker may replay a leg after an
//! ambiguous HTTP or local-database failure without a duplicate movement.

use vend_core::money::Charge;
use vend_core::payout::{PayoutExecution, PayoutLeg, PayoutRepo, PayoutState};
use vend_core::transport::{MintTransport, TransferRequest};
use vend_core::{ChargeId, VendError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayoutConfig {
    pub clearing_account_id: String,
    pub provider_account_id: String,
    pub platform_account_id: String,
}

impl PayoutConfig {
    pub fn new(
        clearing_account_id: impl Into<String>,
        provider_account_id: impl Into<String>,
        platform_account_id: impl Into<String>,
    ) -> Result<Self, VendError> {
        let config = Self {
            clearing_account_id: clearing_account_id.into().trim().to_string(),
            provider_account_id: provider_account_id.into().trim().to_string(),
            platform_account_id: platform_account_id.into().trim().to_string(),
        };
        let accounts = [
            config.clearing_account_id.trim(),
            config.provider_account_id.trim(),
            config.platform_account_id.trim(),
        ];
        if accounts.iter().any(|account| account.is_empty()) {
            return Err(VendError::Config(
                "payout clearing, provider, and platform Mint accounts are required".into(),
            ));
        }
        if accounts[0] == accounts[1] || accounts[0] == accounts[2] || accounts[1] == accounts[2] {
            return Err(VendError::Config(
                "payout clearing, provider, and platform Mint accounts must be distinct".into(),
            ));
        }
        Ok(config)
    }
}

fn validate_conservation(charge: &Charge) -> Result<(), VendError> {
    let parts = [
        charge.provider_cost_micros,
        charge.tenant_net_micros,
        charge.platform_fee_micros,
    ];
    if charge.charge_micros < 0 || parts.iter().any(|amount| *amount < 0) {
        return Err(VendError::InvalidAmount);
    }
    let distributed = parts
        .into_iter()
        .try_fold(0_i64, i64::checked_add)
        .ok_or(VendError::Overflow)?;
    if distributed != charge.charge_micros {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

/// Persist the immutable payout plan before capture.
pub async fn prepare_payout(
    repo: &dyn PayoutRepo,
    charge_id: &ChargeId,
    idem_key: &str,
    config: &PayoutConfig,
    tenant_account_id: &str,
    charge: Charge,
) -> Result<PayoutExecution, VendError> {
    validate_conservation(&charge)?;
    let tenant_account_id = tenant_account_id.trim();
    if tenant_account_id.is_empty() || tenant_account_id == config.clearing_account_id {
        return Err(VendError::Config(
            "tenant payout Mint account is required and cannot be the clearing account".into(),
        ));
    }
    let payout = PayoutExecution {
        charge_id: charge_id.clone(),
        idem_key: idem_key.to_string(),
        clearing_account_id: config.clearing_account_id.clone(),
        provider_account_id: config.provider_account_id.clone(),
        tenant_account_id: tenant_account_id.to_string(),
        platform_account_id: config.platform_account_id.clone(),
        provider_cost_micros: charge.provider_cost_micros,
        tenant_net_micros: charge.tenant_net_micros,
        platform_fee_micros: charge.platform_fee_micros,
        captured_charge_micros: charge.charge_micros,
        provider_transferred: charge.provider_cost_micros == 0,
        tenant_transferred: charge.tenant_net_micros == 0,
        platform_transferred: charge.platform_fee_micros == 0,
        state: PayoutState::Pending,
        last_error: None,
    };
    repo.prepare_payout(&payout).await?;
    Ok(payout)
}

fn leg_details(payout: &PayoutExecution, leg: PayoutLeg) -> (&str, i64, &'static str) {
    match leg {
        PayoutLeg::Provider => (
            &payout.provider_account_id,
            payout.provider_cost_micros,
            "provider",
        ),
        PayoutLeg::Tenant => (
            &payout.tenant_account_id,
            payout.tenant_net_micros,
            "tenant",
        ),
        PayoutLeg::Platform => (
            &payout.platform_account_id,
            payout.platform_fee_micros,
            "platform",
        ),
    }
}

fn leg_done(payout: &PayoutExecution, leg: PayoutLeg) -> bool {
    match leg {
        PayoutLeg::Provider => payout.provider_transferred,
        PayoutLeg::Tenant => payout.tenant_transferred,
        PayoutLeg::Platform => payout.platform_transferred,
    }
}

/// Drive all remaining legs of a prepared plan. Safe under retries and even
/// concurrent executors because Mint owns idempotency for every movement.
pub async fn execute_payout(
    mint: &dyn MintTransport,
    repo: &dyn PayoutRepo,
    charge_id: &ChargeId,
) -> Result<(), VendError> {
    let payout = repo
        .load_payout(charge_id)
        .await?
        .ok_or(VendError::NotFound)?;
    match payout.state {
        PayoutState::Complete => return Ok(()),
        PayoutState::Pending => {}
        PayoutState::Cancelled => {
            return Err(VendError::Storage(
                "cancelled payout plan cannot execute".into(),
            ));
        }
    }
    for leg in [PayoutLeg::Provider, PayoutLeg::Tenant, PayoutLeg::Platform] {
        if leg_done(&payout, leg) {
            continue;
        }
        let (to, amount_micros, suffix) = leg_details(&payout, leg);
        if amount_micros > 0 {
            let result = mint
                .transfer(TransferRequest {
                    from: payout.clearing_account_id.clone(),
                    to: to.to_string(),
                    amount_micros,
                    idem_key: format!("{}:payout:{suffix}", payout.idem_key),
                    ref_: Some(format!("vend:{}:{suffix}", charge_id.as_str())),
                })
                .await;
            if let Err(error) = result {
                let _ = repo
                    .record_payout_error(charge_id, &error.to_string())
                    .await;
                return Err(error);
            }
        }
        repo.mark_payout_leg(charge_id, leg).await?;
    }
    repo.mark_payout_complete(charge_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use vend_testkit::FakeMint;

    fn charge() -> Charge {
        Charge {
            provider_cost_micros: 700,
            markup_micros: 300,
            charge_micros: 1_000,
            platform_fee_micros: 60,
            tenant_net_micros: 240,
        }
    }

    #[test]
    fn accepts_exact_conservation_and_rejects_drift() {
        assert_eq!(validate_conservation(&charge()), Ok(()));
        let mut invalid = charge();
        invalid.tenant_net_micros += 1;
        assert_eq!(
            validate_conservation(&invalid),
            Err(VendError::InvalidAmount)
        );
    }

    #[test]
    fn global_destinations_fail_closed() {
        assert!(PayoutConfig::new("", "provider", "platform").is_err());
        assert!(PayoutConfig::new("same", "same", "platform").is_err());
        assert!(PayoutConfig::new("clearing", "provider", "platform").is_ok());
    }

    struct FailAfterFirstTransfer {
        payout: Mutex<Option<PayoutExecution>>,
        fail_provider_mark_once: Mutex<bool>,
    }

    impl FailAfterFirstTransfer {
        fn new() -> Self {
            Self {
                payout: Mutex::new(None),
                fail_provider_mark_once: Mutex::new(true),
            }
        }
    }

    #[async_trait]
    impl PayoutRepo for FailAfterFirstTransfer {
        async fn prepare_payout(&self, payout: &PayoutExecution) -> Result<(), VendError> {
            *self.payout.lock().unwrap() = Some(payout.clone());
            Ok(())
        }

        async fn load_payout(
            &self,
            _charge_id: &ChargeId,
        ) -> Result<Option<PayoutExecution>, VendError> {
            Ok(self.payout.lock().unwrap().clone())
        }

        async fn pending_payouts_before(
            &self,
            _older_than_unix_seconds: i64,
            _limit: u32,
        ) -> Result<Vec<PayoutExecution>, VendError> {
            Ok(self
                .payout
                .lock()
                .unwrap()
                .iter()
                .filter(|payout| payout.state == PayoutState::Pending)
                .cloned()
                .collect())
        }

        async fn mark_payout_leg(
            &self,
            _charge_id: &ChargeId,
            leg: PayoutLeg,
        ) -> Result<(), VendError> {
            if leg == PayoutLeg::Provider {
                let mut fail = self.fail_provider_mark_once.lock().unwrap();
                if *fail {
                    *fail = false;
                    return Err(VendError::Storage("ambiguous local commit".into()));
                }
            }
            let mut payout = self.payout.lock().unwrap();
            let payout = payout.as_mut().unwrap();
            match leg {
                PayoutLeg::Provider => payout.provider_transferred = true,
                PayoutLeg::Tenant => payout.tenant_transferred = true,
                PayoutLeg::Platform => payout.platform_transferred = true,
            }
            Ok(())
        }

        async fn mark_payout_complete(&self, _charge_id: &ChargeId) -> Result<(), VendError> {
            self.payout.lock().unwrap().as_mut().unwrap().state = PayoutState::Complete;
            Ok(())
        }

        async fn record_payout_error(
            &self,
            _charge_id: &ChargeId,
            message: &str,
        ) -> Result<(), VendError> {
            self.payout.lock().unwrap().as_mut().unwrap().last_error = Some(message.into());
            Ok(())
        }
    }

    #[tokio::test]
    async fn replay_after_local_mark_failure_does_not_double_transfer() {
        let repo = FailAfterFirstTransfer::new();
        let mint = FakeMint::new().with_balance("clearing", 1_000);
        let charge_id = ChargeId::generate();
        let config = PayoutConfig::new("clearing", "provider", "platform").unwrap();
        prepare_payout(&repo, &charge_id, "vend:test", &config, "tenant", charge())
            .await
            .unwrap();

        assert!(execute_payout(&mint, &repo, &charge_id).await.is_err());
        assert_eq!(mint.balance("provider"), 700);

        execute_payout(&mint, &repo, &charge_id).await.unwrap();
        assert_eq!(mint.balance("provider"), 700);
        assert_eq!(mint.balance("tenant"), 240);
        assert_eq!(mint.balance("platform"), 60);
        assert_eq!(mint.balance("clearing"), 0);
        assert_eq!(
            repo.load_payout(&charge_id).await.unwrap().unwrap().state,
            PayoutState::Complete
        );
    }

    #[tokio::test]
    async fn cancelled_plan_can_never_move_clearing_funds() {
        let repo = FailAfterFirstTransfer::new();
        let mint = FakeMint::new().with_balance("clearing", 1_000);
        let charge_id = ChargeId::generate();
        let config = PayoutConfig::new("clearing", "provider", "platform").unwrap();
        prepare_payout(
            &repo,
            &charge_id,
            "vend:cancelled",
            &config,
            "tenant",
            charge(),
        )
        .await
        .unwrap();
        repo.payout.lock().unwrap().as_mut().unwrap().state = PayoutState::Cancelled;

        assert!(execute_payout(&mint, &repo, &charge_id).await.is_err());
        assert_eq!(mint.balance("clearing"), 1_000);
        assert_eq!(mint.balance("provider"), 0);
        assert_eq!(mint.balance("tenant"), 0);
        assert_eq!(mint.balance("platform"), 0);
    }
}

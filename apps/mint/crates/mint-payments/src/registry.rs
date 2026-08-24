//! [`PaymentRegistry`] — a config/env-key gated provider table mirroring the
//! BYOK routing pattern (unset key ⇒ inactive), built once at startup by
//! [`PaymentRegistry::from_env`].
//!
//! - `ramps`: only providers whose `api_key` is present (each an adapter over
//!   the shared [`HttpTransport`]).
//! - `webhook_secrets`: only providers whose `webhook_secret` is present (a
//!   missing secret ⇒ the route returns `"provider not configured"`).
//! - `composite`: a [`CompositeRamp`] over `ramps`, or a deterministic
//!   [`FakeRamp`] when NO provider is configured (dev/test still works).

use std::collections::HashMap;
use std::sync::Arc;

use mint_reserve::{FakeRamp, Ramp};

use crate::card::{CreemRamp, PayPalRamp, PolarRamp, StripeRamp};
use crate::composite::CompositeRamp;
use crate::config::{PaymentsConfig, ProviderConfig};
use crate::crypto_tr::{IyzicoRamp, UsdcRamp};
use crate::event::{DepositEvent, WebhookError};
use crate::http::{Headers, HttpTransport};
use crate::iap::{AdaptyRamp, RevenueCatRamp};
use crate::provider::ProviderId;

/// The startup provider table injected into `mint-server`'s `AppState`.
pub struct PaymentRegistry {
    ramps: HashMap<ProviderId, Arc<dyn Ramp>>,
    webhook_secrets: HashMap<ProviderId, ProviderConfig>,
    composite: Arc<dyn Ramp>,
    /// The shared transport, retained so the async (transport-backed) PayPal
    /// webhook verification can be dispatched from [`PaymentRegistry::verify_webhook`].
    transport: Arc<dyn HttpTransport>,
}

impl PaymentRegistry {
    /// Build from the process environment + a shared transport.
    pub fn from_env(transport: Arc<dyn HttpTransport>) -> Self {
        Self::from_config(PaymentsConfig::from_env(), transport)
    }

    /// Build from an explicit config (used by tests without touching the env).
    pub fn from_config(cfg: PaymentsConfig, transport: Arc<dyn HttpTransport>) -> Self {
        let mut ramps: HashMap<ProviderId, Arc<dyn Ramp>> = HashMap::new();
        let mut webhook_secrets: HashMap<ProviderId, ProviderConfig> = HashMap::new();

        for pc in cfg.iter() {
            if pc.is_active() {
                ramps.insert(pc.provider, build_ramp(pc.clone(), transport.clone()));
            }
            if pc.has_webhook_secret() {
                webhook_secrets.insert(pc.provider, pc.clone());
            }
        }

        let composite: Arc<dyn Ramp> = if ramps.is_empty() {
            // BYOK: nothing configured ⇒ preserve today's FakeRamp behavior.
            Arc::new(FakeRamp::new())
        } else {
            Arc::new(CompositeRamp::new(ramps.clone()))
        };

        PaymentRegistry {
            ramps,
            webhook_secrets,
            composite,
            transport,
        }
    }

    /// Verify a provider webhook over its RAW bytes and, on success, return the
    /// confirmed [`DepositEvent`] the caller feeds into `reserve.mint`. Dispatch
    /// is by [`ProviderId`]; signature schemes live entirely in the per-provider
    /// adapters. A provider with no configured signing secret ⇒
    /// [`WebhookError::NotConfigured`] (the route maps this to 500).
    pub async fn verify_webhook<H: Headers>(
        &self,
        provider: ProviderId,
        headers: &H,
        raw: &[u8],
    ) -> Result<DepositEvent, WebhookError> {
        let cfg = self
            .webhook_secrets
            .get(&provider)
            .ok_or(WebhookError::NotConfigured)?;
        match provider {
            ProviderId::Stripe => crate::card::stripe::verify_webhook(cfg, headers, raw),
            ProviderId::Creem => crate::card::creem::verify_webhook(cfg, headers, raw),
            ProviderId::Polar => crate::card::polar::verify_webhook(cfg, headers, raw),
            // PayPal verifies by echoing the event back to PayPal (transport-backed).
            ProviderId::PayPal => {
                PayPalRamp::new(self.transport.clone(), cfg.clone())
                    .verify_webhook(headers, raw)
                    .await
            }
            ProviderId::RevenueCat => crate::iap::revenuecat::verify_webhook(cfg, headers, raw),
            ProviderId::Adapty => crate::iap::adapty::verify_webhook(cfg, headers, raw),
            ProviderId::Usdc => crate::crypto_tr::usdc::verify_webhook(cfg, headers, raw),
            ProviderId::Iyzico => crate::crypto_tr::iyzico::verify_webhook(cfg, headers, raw),
        }
    }

    /// The composite ramp injected as `AppState.reserve`'s [`Ramp`]. Falls back
    /// to [`FakeRamp`] when the registry is empty.
    pub fn ramp(&self) -> Arc<dyn Ramp> {
        self.composite.clone()
    }

    /// The signing config for a provider's webhook route. `None` ⇒ the route
    /// returns `Internal("provider not configured")` (the Stripe empty-secret
    /// path).
    pub fn webhook_config(&self, provider: ProviderId) -> Option<&ProviderConfig> {
        self.webhook_secrets.get(&provider)
    }

    /// `true` iff the provider has an active on/off-ramp adapter.
    pub fn is_active(&self, provider: ProviderId) -> bool {
        self.ramps.contains_key(&provider)
    }

    /// The count of active adapters (for diagnostics / logging).
    pub fn active_count(&self) -> usize {
        self.ramps.len()
    }
}

/// Construct the concrete adapter for an active provider config.
fn build_ramp(cfg: ProviderConfig, transport: Arc<dyn HttpTransport>) -> Arc<dyn Ramp> {
    match cfg.provider {
        ProviderId::Stripe => Arc::new(StripeRamp::new(transport, cfg)),
        ProviderId::Creem => Arc::new(CreemRamp::new(transport, cfg)),
        ProviderId::Polar => Arc::new(PolarRamp::new(transport, cfg)),
        ProviderId::PayPal => Arc::new(PayPalRamp::new(transport, cfg)),
        ProviderId::RevenueCat => Arc::new(RevenueCatRamp::new(transport, cfg)),
        ProviderId::Adapty => Arc::new(AdaptyRamp::new(transport, cfg)),
        ProviderId::Usdc => Arc::new(UsdcRamp::new(transport, cfg)),
        ProviderId::Iyzico => Arc::new(IyzicoRamp::new(transport, cfg)),
    }
}

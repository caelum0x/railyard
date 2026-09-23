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

use mint_reserve::{Deposit, FakeRamp, Ramp};

use crate::card::{Checkout, CheckoutUrls};
use crate::card::{CreemRamp, PayPalRamp, PolarRamp, StripeRamp};
use crate::composite::CompositeRamp;
use crate::config::{PaymentsConfig, ProviderConfig};
use crate::crypto_tr::{IyzicoRamp, UsdcRamp};
use crate::event::{DepositEvent, WebhookError};
use crate::http::{Headers, HttpTransport};
use crate::iap::{AdaptyRamp, RevenueCatRamp};
use crate::provider::ProviderId;
use crate::PaymentError;

/// The startup provider table injected into `mint-server`'s `AppState`.
pub struct PaymentRegistry {
    ramps: HashMap<ProviderId, Arc<dyn Ramp>>,
    webhook_secrets: HashMap<ProviderId, ProviderConfig>,
    composite: Arc<dyn Ramp>,
    /// The shared transport, retained so the async (transport-backed) PayPal
    /// webhook verification can be dispatched from [`PaymentRegistry::verify_webhook`].
    transport: Arc<dyn HttpTransport>,
    active_configs: HashMap<ProviderId, ProviderConfig>,
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
            active_configs: cfg
                .iter()
                .filter(|provider| provider.is_active())
                .map(|provider| (provider.provider, provider.clone()))
                .collect(),
        }
    }

    /// Create a provider-hosted checkout bound to a Mint account. The later
    /// signed webhook carries that account id back and deposits directly into
    /// Mint; callers never credit a parallel application ledger.
    pub async fn create_checkout(
        &self,
        provider: ProviderId,
        account_id: &str,
        amount_micros: i64,
        urls: &CheckoutUrls,
    ) -> Result<Checkout, PaymentError> {
        let cfg = self
            .active_configs
            .get(&provider)
            .cloned()
            .ok_or(PaymentError::NotConfigured(provider))?;
        let checkout = match provider {
            ProviderId::Stripe => {
                StripeRamp::new(self.transport.clone(), cfg)
                    .create_checkout(account_id, amount_micros, urls)
                    .await?
            }
            ProviderId::Creem => {
                CreemRamp::new(self.transport.clone(), cfg)
                    .create_checkout(account_id, amount_micros, urls)
                    .await?
            }
            ProviderId::Polar => {
                PolarRamp::new(self.transport.clone(), cfg)
                    .create_checkout(account_id, amount_micros, urls)
                    .await?
            }
            ProviderId::PayPal => {
                if urls.capture_return_url.is_none() {
                    return Err(PaymentError::Misconfigured(
                        "MINT_PUBLIC_URL must be set so PayPal can return the buyer to Mint's \
                         capture endpoint"
                            .into(),
                    ));
                }
                PayPalRamp::new(self.transport.clone(), cfg)
                    .create_checkout(account_id, amount_micros, urls)
                    .await?
            }
            ProviderId::Usdc => {
                UsdcRamp::new(self.transport.clone(), cfg)
                    .create_charge(account_id, amount_micros)
                    .await?
            }
            ProviderId::Iyzico => {
                IyzicoRamp::new(self.transport.clone(), cfg)
                    .initialize_checkout(account_id, amount_micros)
                    .await?
            }
            ProviderId::RevenueCat | ProviderId::Adapty => {
                return Err(PaymentError::NotConfigured(provider));
            }
        };
        if checkout.checkout_id.trim().is_empty() || checkout.checkout_url.trim().is_empty() {
            return Err(PaymentError::Transport(
                "provider returned an incomplete checkout".into(),
            ));
        }
        Ok(checkout)
    }

    /// Capture an approved PayPal order (the buyer returned from PayPal) and
    /// return the authenticated, confirmed deposit keyed by the ORDER id.
    pub async fn capture_paypal_order(&self, order_id: &str) -> Result<Deposit, PaymentError> {
        let cfg = self
            .active_configs
            .get(&ProviderId::PayPal)
            .cloned()
            .ok_or(PaymentError::NotConfigured(ProviderId::PayPal))?;
        PayPalRamp::new(self.transport.clone(), cfg)
            .capture_order(order_id)
            .await
            .map_err(PaymentError::from)
    }

    /// The tags of every active provider (for the operator dashboard).
    pub fn active_providers(&self) -> Vec<ProviderId> {
        let mut v: Vec<ProviderId> = self.ramps.keys().copied().collect();
        v.sort_by_key(|p| p.tag());
        v
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::http::{HttpResponse, TransportError};

    #[derive(Default)]
    struct CheckoutTransport {
        bodies: Mutex<Vec<Vec<u8>>>,
    }

    #[async_trait]
    impl HttpTransport for CheckoutTransport {
        async fn get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> Result<HttpResponse, TransportError> {
            Err(TransportError("unexpected get".into()))
        }

        async fn post_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            body: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            self.bodies.lock().unwrap().push(body.to_vec());
            Ok(HttpResponse {
                status: 200,
                body: br#"{"id":"checkout_1","checkout_url":"https://pay.test/1"}"#.to_vec(),
            })
        }
    }

    #[tokio::test]
    async fn checkout_binds_mint_account_in_provider_metadata() {
        let config = PaymentsConfig::from_lookup(|key| match key {
            "CREEM_API_KEY" => Some("creem_test".to_string()),
            "CREEM_PRODUCT_ID" => Some("prod_ait".to_string()),
            _ => None,
        });
        let transport = Arc::new(CheckoutTransport::default());
        let registry = PaymentRegistry::from_config(config, transport.clone());
        let urls = CheckoutUrls {
            success_url: "https://app.test/ok".into(),
            cancel_url: "https://app.test/cancel".into(),
            capture_return_url: None,
        };

        let checkout = registry
            .create_checkout(ProviderId::Creem, "acct_user", 5_000_000, &urls)
            .await
            .unwrap();
        assert_eq!(checkout.checkout_id, "checkout_1");
        let bodies = transport.bodies.lock().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bodies[0]).unwrap();
        assert_eq!(body["metadata"]["account_id"], "acct_user");
        assert_eq!(body["product_id"], "prod_ait");
        // $5.00 at the default $1.00 unit price ⇒ 5 units.
        assert_eq!(body["units"], 5);
        assert_eq!(body["success_url"], "https://app.test/ok");
    }
}

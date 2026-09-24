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
use crate::event::{DepositEvent, PayoutEvent, ReversalEvent, VerifiedEvent, WebhookError};
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
                let base = urls
                    .public_base_url
                    .as_deref()
                    .map(|b| b.trim_end_matches('/'))
                    .filter(|b| !b.is_empty())
                    .ok_or_else(|| {
                        PaymentError::Misconfigured(
                            "MINT_PUBLIC_URL must be set so Iyzico can return the buyer to \
                             Mint's /v1/deposits/iyzico/webhook callback"
                                .into(),
                        )
                    })?;
                let callback_url = format!("{base}/v1/deposits/iyzico/webhook");
                IyzicoRamp::new(self.transport.clone(), cfg)
                    .initialize_checkout(account_id, amount_micros, &callback_url)
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

    /// Verify a provider webhook ONCE and classify it as a deposit to mint or
    /// a reversal (refund / chargeback) of a deposit Mint already minted.
    ///
    /// Reversal mappings: Stripe (`charge.refunded`, `charge.dispute.*`),
    /// PayPal (`PAYMENT.CAPTURE.REFUNDED|REVERSED`, `CUSTOMER.DISPUTE.CREATED`),
    /// Creem (`refund.created`, `dispute.created`), Polar (`order.refunded`),
    /// RevenueCat (`CANCELLATION` with `CUSTOMER_SUPPORT`) and Adapty
    /// (`subscription_refunded`, `non_subscription_purchase_refunded`). USDC
    /// transfers are final (no reversal), and Iyzico refund / cancel /
    /// chargeback merchant notifications (`X-IYZ-SIGNATURE-V3`). Any other
    /// verified event is `Ignored`.
    pub async fn verify_event<H: Headers>(
        &self,
        provider: ProviderId,
        headers: &H,
        raw: &[u8],
    ) -> Result<VerifiedEvent, WebhookError> {
        let cfg = self
            .webhook_secrets
            .get(&provider)
            .ok_or(WebhookError::NotConfigured)?;
        if provider == ProviderId::PayPal {
            // Transport-backed verification: authenticate exactly once, then
            // classify from the verified payload.
            return PayPalRamp::new(self.transport.clone(), cfg.clone())
                .verify_event(headers, raw)
                .await;
        }
        if provider == ProviderId::Iyzico {
            // The checkout-form callback carries only a token that must be
            // resolved through a signed API call; merchant notifications are
            // HMAC-verified and may be refunds. Use the ACTIVE config (it holds
            // the API + secret key the signed calls need).
            let active = self
                .active_configs
                .get(&ProviderId::Iyzico)
                .cloned()
                .ok_or(WebhookError::NotConfigured)?;
            return IyzicoRamp::new(self.transport.clone(), active)
                .verify_event(headers, raw)
                .await;
        }
        match self.verify_webhook(provider, headers, raw).await {
            Ok(ev) => Ok(VerifiedEvent::Deposit(ev)),
            Err(WebhookError::Ignored(kind)) => {
                match verify_reversal_sync(provider, cfg, headers, raw) {
                    Ok(rev) => Ok(VerifiedEvent::Reversal(rev)),
                    // Not a reversal: maybe a payout settlement update.
                    Err(WebhookError::Ignored(_)) => {
                        match verify_payout_sync(provider, cfg, headers, raw) {
                            Ok(p) => Ok(VerifiedEvent::Payout(p)),
                            // None of the three: report the original event kind.
                            Err(WebhookError::Ignored(_)) => Err(WebhookError::Ignored(kind)),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
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

/// Classify an already-`Ignored` (non-deposit) delivery of a locally-verified
/// provider as a reversal. Each adapter re-checks its (cheap, local) signature.
fn verify_reversal_sync<H: Headers>(
    provider: ProviderId,
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<ReversalEvent, WebhookError> {
    match provider {
        ProviderId::Stripe => crate::card::stripe::verify_reversal_webhook(cfg, headers, raw),
        ProviderId::Creem => crate::card::creem::verify_reversal_webhook(cfg, headers, raw),
        ProviderId::Polar => crate::card::polar::verify_reversal_webhook(cfg, headers, raw),
        ProviderId::RevenueCat => {
            crate::iap::revenuecat::verify_reversal_webhook(cfg, headers, raw)
        }
        ProviderId::Adapty => crate::iap::adapty::verify_reversal_webhook(cfg, headers, raw),
        ProviderId::Usdc => Err(WebhookError::Ignored(
            "usdc transfers are final and cannot be reversed".into(),
        )),
        // Handled asynchronously in `verify_event` (transport-backed verify).
        ProviderId::Iyzico => Err(WebhookError::Ignored("iyzico".into())),
        ProviderId::PayPal => Err(WebhookError::Ignored("paypal".into())),
    }
}

/// Classify a locally-verified delivery as a payout settlement update. Only
/// Stripe (transfer reversals) is synchronous; PayPal payout items are handled
/// in its transport-backed `verify_event`, and the IAP / MoR providers have no
/// payouts.
fn verify_payout_sync<H: Headers>(
    provider: ProviderId,
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<PayoutEvent, WebhookError> {
    match provider {
        ProviderId::Stripe => crate::card::stripe::verify_payout_webhook(cfg, headers, raw),
        other => Err(WebhookError::Ignored(format!("{} has no payout events", other.tag()))),
    }
}

/// What an active provider can do, for the operator console's provider picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// The provider.
    pub provider: ProviderId,
    /// A hosted checkout can be created (`POST /v1/checkouts`).
    pub checkout: bool,
    /// Redeems can pay out through it.
    pub payout: bool,
    /// Its webhook route is configured (deposits / reversals are received).
    pub webhook: bool,
}

impl PaymentRegistry {
    /// The capabilities of every ACTIVE provider, sorted by tag.
    pub fn capabilities(&self) -> Vec<ProviderCapabilities> {
        self.active_providers()
            .into_iter()
            .map(|provider| {
                let cfg = self.active_configs.get(&provider);
                let extra = |k: &str| cfg.and_then(|c| c.extra(k)).is_some_and(|v| !v.is_empty());
                let (checkout, payout) = match provider {
                    ProviderId::Stripe | ProviderId::PayPal => (true, true),
                    ProviderId::Creem => (extra("product_id"), false),
                    ProviderId::Polar => (extra("product_id"), false),
                    ProviderId::Usdc => (
                        extra("commerce_api_key"),
                        extra("rpc_url") && extra("token_contract"),
                    ),
                    ProviderId::Iyzico => (extra("secret_key"), extra("secret_key")),
                    ProviderId::RevenueCat | ProviderId::Adapty => (false, false),
                };
                ProviderCapabilities {
                    provider,
                    checkout,
                    payout,
                    webhook: self.webhook_secrets.contains_key(&provider),
                }
            })
            .collect()
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
            public_base_url: None,
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

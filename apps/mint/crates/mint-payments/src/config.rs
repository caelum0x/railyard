//! Per-provider configuration, env-key gated (BYOK). An unset/empty `api_key`
//! ⇒ the provider is inactive (dropped from the registry's `ramps`); an
//! unset/empty `webhook_secret` ⇒ its webhook route returns
//! `"provider not configured"`. This mirrors the existing Stripe empty-secret
//! guard exactly.

use std::collections::HashMap;

use crate::provider::ProviderId;

/// Sandbox vs live provider mode (selects base URLs for PayPal / Iyzico).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Sandbox / test base URLs.
    Sandbox,
    /// Live / production base URLs.
    Live,
}

impl Mode {
    fn from_env_str(s: &str) -> Mode {
        if s.eq_ignore_ascii_case("live") || s.eq_ignore_ascii_case("production") {
            Mode::Live
        } else {
            Mode::Sandbox
        }
    }
}

/// One provider's resolved configuration. `None` fields mean "not supplied".
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// The provider this config is for.
    pub provider: ProviderId,
    /// The provider API key / secret key / access token / RPC url. `None` ⇒
    /// provider inactive (no adapter is built).
    pub api_key: Option<String>,
    /// The webhook signing secret / shared auth. `None` ⇒ the webhook route is
    /// not configured (500).
    pub webhook_secret: Option<String>,
    /// Sandbox / live mode.
    pub mode: Mode,
    /// Provider-specific extras (client secret, secret key, token contract, …).
    pub extra: HashMap<String, String>,
}

impl ProviderConfig {
    /// An empty (inactive) config for `provider`.
    pub fn inactive(provider: ProviderId) -> Self {
        ProviderConfig {
            provider,
            api_key: None,
            webhook_secret: None,
            mode: Mode::Sandbox,
            extra: HashMap::new(),
        }
    }

    /// `true` iff an `api_key` is present and non-empty (⇒ an adapter is built).
    pub fn is_active(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }

    /// `true` iff a `webhook_secret` is present and non-empty.
    pub fn has_webhook_secret(&self) -> bool {
        self.webhook_secret
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    }

    /// Look up a provider-specific extra by key.
    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extra.get(key).map(|s| s.as_str())
    }

    /// Builder: set the api key from an env value (empty ⇒ `None`).
    fn with_api_key(mut self, v: Option<String>) -> Self {
        self.api_key = non_empty(v);
        self
    }

    fn with_webhook_secret(mut self, v: Option<String>) -> Self {
        self.webhook_secret = non_empty(v);
        self
    }

    fn with_extra(mut self, key: &str, v: Option<String>) -> Self {
        if let Some(val) = non_empty(v) {
            self.extra.insert(key.to_string(), val);
        }
        self
    }
}

/// The full payments configuration: one [`ProviderConfig`] per known provider.
#[derive(Clone, Debug)]
pub struct PaymentsConfig {
    providers: HashMap<ProviderId, ProviderConfig>,
}

impl PaymentsConfig {
    /// Build from an explicit env-like lookup (used by [`PaymentsConfig::from_env`]
    /// and by tests without touching the process environment).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mode = lookup("MINT_PAYMENTS_MODE")
            .map(|s| Mode::from_env_str(&s))
            .unwrap_or(Mode::Sandbox);
        let mut providers = HashMap::new();
        let g = |k: &str| lookup(k);

        providers.insert(
            ProviderId::Stripe,
            base(ProviderId::Stripe, mode)
                .with_api_key(g("STRIPE_API_KEY"))
                .with_webhook_secret(g("STRIPE_WEBHOOK_SECRET")),
        );
        providers.insert(
            ProviderId::Creem,
            base(ProviderId::Creem, mode)
                .with_api_key(g("CREEM_API_KEY"))
                // Creem checkouts are product-based: a fixed-price "AIT credit"
                // product bought in `units` (amount = units × unit price).
                .with_extra("product_id", g("CREEM_PRODUCT_ID"))
                .with_extra("unit_price_cents", g("CREEM_UNIT_PRICE_CENTS"))
                .with_webhook_secret(g("CREEM_WEBHOOK_SECRET")),
        );
        providers.insert(
            ProviderId::Polar,
            base(ProviderId::Polar, mode)
                .with_api_key(g("POLAR_ACCESS_TOKEN"))
                // A pay-what-you-want (custom price) Polar product; the checkout
                // sets `amount` per deposit.
                .with_extra("product_id", g("POLAR_PRODUCT_ID"))
                .with_webhook_secret(g("POLAR_WEBHOOK_SECRET")),
        );
        providers.insert(
            ProviderId::PayPal,
            base(ProviderId::PayPal, mode)
                .with_api_key(g("PAYPAL_CLIENT_ID"))
                .with_extra("client_secret", g("PAYPAL_CLIENT_SECRET"))
                .with_webhook_secret(g("PAYPAL_WEBHOOK_ID")),
        );
        providers.insert(
            ProviderId::RevenueCat,
            base(ProviderId::RevenueCat, mode)
                .with_api_key(g("REVENUECAT_API_KEY"))
                .with_webhook_secret(g("REVENUECAT_WEBHOOK_AUTH")),
        );
        providers.insert(
            ProviderId::Adapty,
            base(ProviderId::Adapty, mode)
                .with_api_key(g("ADAPTY_API_KEY"))
                .with_webhook_secret(g("ADAPTY_WEBHOOK_SECRET")),
        );
        providers.insert(
            ProviderId::Usdc,
            base(ProviderId::Usdc, mode)
                // The adapter is active when EITHER settlement path is configured:
                // the JSON-RPC node (on-chain confirm + payouts) or the Coinbase
                // Commerce key (hosted charges re-confirmed via GET /charges).
                .with_api_key(g("USDC_RPC_URL").or_else(|| g("COINBASE_COMMERCE_API_KEY")))
                .with_extra("rpc_url", g("USDC_RPC_URL"))
                .with_extra("token_contract", g("USDC_TOKEN_CONTRACT"))
                .with_extra("payout_from", g("USDC_PAYOUT_FROM"))
                // The treasury address raw on-chain deposits are sent to (the
                // ERC-20 Transfer log recipient a tx-hash deposit must credit).
                .with_extra("deposit_address", g("USDC_DEPOSIT_ADDRESS"))
                .with_extra("commerce_api_key", g("COINBASE_COMMERCE_API_KEY"))
                .with_webhook_secret(g("COINBASE_COMMERCE_WEBHOOK_SECRET")),
        );
        providers.insert(
            ProviderId::Iyzico,
            base(ProviderId::Iyzico, mode)
                .with_api_key(g("IYZICO_API_KEY"))
                .with_extra("secret_key", g("IYZICO_SECRET_KEY"))
                // Iyzico signs merchant notifications (X-IYZ-SIGNATURE-V3) with
                // the merchant SECRET KEY; a dedicated webhook secret is optional
                // and only gates whether the route is enabled.
                .with_webhook_secret(g("IYZICO_WEBHOOK_SECRET").or_else(|| g("IYZICO_SECRET_KEY"))),
        );

        PaymentsConfig { providers }
    }

    /// Build from the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// The config for a provider (always present; may be inactive).
    pub fn get(&self, provider: ProviderId) -> &ProviderConfig {
        self.providers
            .get(&provider)
            .expect("every provider has a config entry")
    }

    /// Iterate every provider config.
    pub fn iter(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.values()
    }
}

fn base(provider: ProviderId, mode: Mode) -> ProviderConfig {
    ProviderConfig {
        provider,
        api_key: None,
        webhook_secret: None,
        mode,
        extra: HashMap::new(),
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_provider_is_inactive() {
        let cfg = PaymentsConfig::from_lookup(|_| None);
        assert!(!cfg.get(ProviderId::Stripe).is_active());
        assert!(!cfg.get(ProviderId::Stripe).has_webhook_secret());
    }

    #[test]
    fn set_keys_activate_provider() {
        let cfg = PaymentsConfig::from_lookup(|k| match k {
            "STRIPE_API_KEY" => Some("sk_test_123".into()),
            "STRIPE_WEBHOOK_SECRET" => Some("whsec_abc".into()),
            _ => None,
        });
        let sc = cfg.get(ProviderId::Stripe);
        assert!(sc.is_active());
        assert!(sc.has_webhook_secret());
        assert_eq!(sc.webhook_secret.as_deref(), Some("whsec_abc"));
    }

    #[test]
    fn empty_string_counts_as_unset() {
        let cfg = PaymentsConfig::from_lookup(|k| match k {
            "STRIPE_API_KEY" => Some("   ".into()),
            _ => None,
        });
        assert!(!cfg.get(ProviderId::Stripe).is_active());
    }

    #[test]
    fn paypal_extras_and_mode() {
        let cfg = PaymentsConfig::from_lookup(|k| match k {
            "MINT_PAYMENTS_MODE" => Some("live".into()),
            "PAYPAL_CLIENT_ID" => Some("id".into()),
            "PAYPAL_CLIENT_SECRET" => Some("secret".into()),
            "PAYPAL_WEBHOOK_ID" => Some("wh_1".into()),
            _ => None,
        });
        let pc = cfg.get(ProviderId::PayPal);
        assert!(pc.is_active());
        assert_eq!(pc.mode, Mode::Live);
        assert_eq!(pc.extra("client_secret"), Some("secret"));
        assert_eq!(pc.webhook_secret.as_deref(), Some("wh_1"));
    }
}

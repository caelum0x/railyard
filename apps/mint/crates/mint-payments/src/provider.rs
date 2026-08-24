//! [`ProviderId`] — the stable provider tag used both to key the registry and to
//! namespace `deposit_ref`/`payout_ref` values (e.g. `"stripe:pi_.."`,
//! `"usdc:0x.."`). The tag is the authoritative dedupe/dispatch key so multiple
//! live providers can coexist behind one [`crate::composite::CompositeRamp`].

/// A supported payment provider. The lowercase [`ProviderId::tag`] is the prefix
/// carried on every `deposit_ref`/`payout_ref` so [`crate::composite`] can fan a
/// confirm/payout out to the right adapter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProviderId {
    /// Stripe (card / merchant-of-record fiat).
    Stripe,
    /// Creem (merchant-of-record, billing-only).
    Creem,
    /// Polar (merchant-of-record, billing-only).
    Polar,
    /// PayPal (card / wallet).
    PayPal,
    /// RevenueCat (mobile IAP aggregator, webhook-only).
    RevenueCat,
    /// Adapty (mobile IAP aggregator, webhook-only).
    Adapty,
    /// USDC on-chain deposit (Coinbase Commerce webhook or chain watcher).
    Usdc,
    /// Iyzico (Turkish card acquirer, 3DS callback).
    Iyzico,
}

impl ProviderId {
    /// Every known provider, for registry construction.
    pub const ALL: [ProviderId; 8] = [
        ProviderId::Stripe,
        ProviderId::Creem,
        ProviderId::Polar,
        ProviderId::PayPal,
        ProviderId::RevenueCat,
        ProviderId::Adapty,
        ProviderId::Usdc,
        ProviderId::Iyzico,
    ];

    /// The lowercase tag used as the `<provider>:` ref prefix.
    pub const fn tag(self) -> &'static str {
        match self {
            ProviderId::Stripe => "stripe",
            ProviderId::Creem => "creem",
            ProviderId::Polar => "polar",
            ProviderId::PayPal => "paypal",
            ProviderId::RevenueCat => "revenuecat",
            ProviderId::Adapty => "adapty",
            ProviderId::Usdc => "usdc",
            ProviderId::Iyzico => "iyzico",
        }
    }

    /// Parse a provider from a ref tag prefix (the text before the first `:`).
    pub fn from_tag(tag: &str) -> Option<ProviderId> {
        ProviderId::ALL.into_iter().find(|p| p.tag() == tag)
    }

    /// Prefix a raw provider ref with `"<tag>:"` (namespaced dedupe key).
    pub fn namespaced(self, raw_ref: &str) -> String {
        format!("{}:{}", self.tag(), raw_ref)
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_round_trips_for_every_provider() {
        for p in ProviderId::ALL {
            assert_eq!(ProviderId::from_tag(p.tag()), Some(p));
        }
        assert_eq!(ProviderId::from_tag("nope"), None);
    }

    #[test]
    fn namespaced_prefixes_the_tag() {
        assert_eq!(ProviderId::Stripe.namespaced("pi_123"), "stripe:pi_123");
    }
}

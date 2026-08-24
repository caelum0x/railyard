//! Registry (BYOK gating) + CompositeRamp dispatch suite.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::FakeTransport;
use mint_core::Micros;
use mint_payments::composite::CompositeRamp;
use mint_payments::config::PaymentsConfig;
use mint_payments::{PaymentRegistry, PayoutDest, ProviderId, Ramp, RampError};
use mint_reserve::{Asset, FakeRamp};

#[tokio::test]
async fn composite_routes_by_provider_prefix() {
    let stripe = Arc::new(FakeRamp::new().with_confirmed_deposit("pi_1", 1_000_000, Asset::Usd));
    let usdc = Arc::new(FakeRamp::new().with_confirmed_deposit("0xabc", 2_000_000, Asset::Usdc));
    let mut ramps: HashMap<ProviderId, Arc<dyn Ramp>> = HashMap::new();
    ramps.insert(ProviderId::Stripe, stripe);
    ramps.insert(ProviderId::Usdc, usdc);
    let composite = CompositeRamp::new(ramps);

    // Routes to the stripe adapter with the RAW (unprefixed) ref.
    let d = composite
        .confirm_deposit("stripe:pi_1")
        .await
        .expect("routed");
    assert_eq!(d.amount_micros, Micros::new(1_000_000));
    let d = composite
        .confirm_deposit("usdc:0xabc")
        .await
        .expect("routed");
    assert_eq!(d.asset, Asset::Usdc);

    // Unknown prefix ⇒ DepositNotFound.
    assert_eq!(
        composite.confirm_deposit("paypal:x").await,
        Err(RampError::DepositNotFound)
    );
    // No prefix ⇒ DepositNotFound.
    assert_eq!(
        composite.confirm_deposit("bareref").await,
        Err(RampError::DepositNotFound)
    );
}

#[tokio::test]
async fn empty_registry_falls_back_to_fake_ramp() {
    let transport = FakeTransport::new().arc();
    let registry = PaymentRegistry::from_config(PaymentsConfig::from_lookup(|_| None), transport);

    assert_eq!(registry.active_count(), 0);
    assert!(!registry.is_active(ProviderId::Stripe));
    assert!(registry.webhook_config(ProviderId::Stripe).is_none());

    // The fallback ramp is a FakeRamp with no deposits ⇒ DepositNotFound.
    let ramp = registry.ramp();
    assert_eq!(
        ramp.confirm_deposit("stripe:pi_1").await,
        Err(RampError::DepositNotFound)
    );
    // The deterministic FakeRamp still records payouts (dev/test convenience).
    let dest = PayoutDest::Stripe {
        account: "x".into(),
    };
    let payout = ramp
        .initiate_payout("stripe:p", &dest, Micros::new(1))
        .await
        .expect("fake ramp pays");
    assert_eq!(payout.status, mint_reserve::PayoutStatus::Paid);
}

#[test]
fn byok_gating_activates_only_configured_providers() {
    let transport = FakeTransport::new().arc();
    let cfg = PaymentsConfig::from_lookup(|k| match k {
        "STRIPE_API_KEY" => Some("sk_test".into()),
        "STRIPE_WEBHOOK_SECRET" => Some("whsec_x".into()),
        // Creem webhook secret only (no api key) ⇒ inactive ramp, but a webhook config.
        "CREEM_WEBHOOK_SECRET" => Some("cs".into()),
        _ => None,
    });
    let registry = PaymentRegistry::from_config(cfg, transport);

    assert!(registry.is_active(ProviderId::Stripe));
    assert!(registry.webhook_config(ProviderId::Stripe).is_some());

    // Creem: has a webhook secret (route configured) but no active ramp.
    assert!(!registry.is_active(ProviderId::Creem));
    assert!(registry.webhook_config(ProviderId::Creem).is_some());

    // Polar: unset entirely.
    assert!(!registry.is_active(ProviderId::Polar));
    assert!(registry.webhook_config(ProviderId::Polar).is_none());
}

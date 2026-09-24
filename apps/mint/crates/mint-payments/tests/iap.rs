//! Mobile IAP group suite: RevenueCat, Adapty. Authorization shared-secret
//! equality (constant time), wrong-Authorization rejection, and no-payout.

mod common;

use std::collections::HashMap;

use common::FakeTransport;
use mint_core::Micros;
use mint_payments::config::{Mode, ProviderConfig};
use mint_payments::iap::{AdaptyRamp, RevenueCatRamp};
use mint_payments::{PayoutDest, ProviderId, Ramp, RampError, WebhookError};

fn cfg(provider: ProviderId, secret: &str) -> ProviderConfig {
    ProviderConfig {
        provider,
        api_key: Some("test_key".into()),
        webhook_secret: Some(secret.into()),
        mode: Mode::Sandbox,
        extra: HashMap::new(),
    }
}

#[test]
fn revenuecat_authorized_purchase_yields_deposit_event() {
    let secret = "rc-shared-secret";
    let body = br#"{"event":{"type":"INITIAL_PURCHASE","id":"evt_1","app_user_id":"acct_9","price":4.99,"price_in_purchased_currency":4.99}}"#;
    let hdrs = common::headers(&[("authorization", secret)]);
    let ev = mint_payments::iap::revenuecat::verify_webhook(
        &cfg(ProviderId::RevenueCat, secret),
        &hdrs,
        body,
    )
    .expect("verifies");
    assert_eq!(ev.provider, ProviderId::RevenueCat);
    assert_eq!(ev.deposit_ref, "evt_1");
    assert_eq!(ev.amount_micros, 4_990_000);
    assert_eq!(ev.account_id, "acct_9");
}

#[test]
fn revenuecat_wrong_authorization_is_rejected() {
    let secret = "rc-shared-secret";
    let body = br#"{"event":{"type":"INITIAL_PURCHASE","id":"e","app_user_id":"a","price_in_purchased_currency":1.0}}"#;
    let hdrs = common::headers(&[("authorization", "WRONG")]);
    assert_eq!(
        mint_payments::iap::revenuecat::verify_webhook(
            &cfg(ProviderId::RevenueCat, secret),
            &hdrs,
            body
        ),
        Err(WebhookError::BadSignature)
    );
}

#[tokio::test]
async fn revenuecat_has_no_payout() {
    let ramp = RevenueCatRamp::new(FakeTransport::new().arc(), cfg(ProviderId::RevenueCat, "s"));
    let dest = PayoutDest::Stripe {
        account: "x".into(),
    };
    assert_eq!(
        ramp.initiate_payout("revenuecat:1", &dest, Micros::new(1))
            .await,
        Err(RampError::ProviderUnavailable)
    );
}

#[test]
fn adapty_authorized_purchase_yields_deposit_event() {
    let secret = "adapty-secret";
    let body = br#"{"event_type":"non_subscription_purchase","event_properties":{"transaction_id":"txn_7","customer_user_id":"acct_11","price_usd":"9.00"}}"#;
    let hdrs = common::headers(&[("authorization", secret)]);
    let ev =
        mint_payments::iap::adapty::verify_webhook(&cfg(ProviderId::Adapty, secret), &hdrs, body)
            .expect("verifies");
    assert_eq!(ev.deposit_ref, "txn_7");
    assert_eq!(ev.amount_micros, 9_000_000);
    assert_eq!(ev.account_id, "acct_11");
}

#[test]
fn adapty_wrong_authorization_is_rejected() {
    let secret = "adapty-secret";
    let body = br#"{"event_type":"subscription_started","event_properties":{"transaction_id":"t","customer_user_id":"a","price_usd":"1.00"}}"#;
    let hdrs = common::headers(&[("authorization", "nope")]);
    assert_eq!(
        mint_payments::iap::adapty::verify_webhook(&cfg(ProviderId::Adapty, secret), &hdrs, body),
        Err(WebhookError::BadSignature)
    );
}

#[tokio::test]
async fn adapty_has_no_payout() {
    let ramp = AdaptyRamp::new(FakeTransport::new().arc(), cfg(ProviderId::Adapty, "s"));
    let dest = PayoutDest::Stripe {
        account: "x".into(),
    };
    assert_eq!(
        ramp.initiate_payout("adapty:1", &dest, Micros::new(1))
            .await,
        Err(RampError::ProviderUnavailable)
    );
}

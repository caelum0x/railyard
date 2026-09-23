//! Card / merchant-of-record group suite: Stripe, Creem, Polar, PayPal.
//! Canned SIGNED payload → `DepositEvent` (happy path), bad-signature rejection
//! before parse, and fake-HTTP re-confirm / payout / verify paths.

mod common;

use std::collections::HashMap;

use common::{hmac_hex, standard_webhooks, stripe_sig, FakeTransport};
use mint_core::Micros;
use mint_payments::card::{stripe::StripeRamp, CreemRamp};
use mint_payments::config::{Mode, ProviderConfig};
use mint_payments::{Asset, PayoutDest, ProviderId, Ramp, RampError};

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
fn stripe_signed_payload_yields_deposit_event() {
    let secret = "whsec_stripe_test";
    let body = br#"{"type":"payment_intent.succeeded","data":{"object":{"id":"pi_123","amount_received":2299,"metadata":{"account_id":"acct_user_1"}}}}"#;
    let hdrs = common::headers(&[(
        "stripe-signature",
        &stripe_sig(secret.as_bytes(), "1700000000", body),
    )]);

    let ev =
        mint_payments::card::stripe::verify_webhook(&cfg(ProviderId::Stripe, secret), &hdrs, body)
            .expect("verifies");
    assert_eq!(ev.provider, ProviderId::Stripe);
    assert_eq!(ev.deposit_ref, "pi_123");
    assert_eq!(ev.amount_micros, 22_990_000); // cents × 10_000
    assert_eq!(ev.asset, Asset::Usd);
    assert_eq!(ev.account_id, "acct_user_1");
    assert_eq!(ev.namespaced_ref(), "stripe:pi_123");
}

#[test]
fn stripe_bad_signature_is_rejected() {
    let secret = "whsec_stripe_test";
    let body = br#"{"type":"payment_intent.succeeded","data":{"object":{"id":"pi_1","amount_received":100,"metadata":{"account_id":"a"}}}}"#;
    let mut sig = stripe_sig(secret.as_bytes(), "1700000000", body);
    sig.pop();
    sig.push('0');
    let hdrs = common::headers(&[("stripe-signature", &sig)]);
    let err =
        mint_payments::card::stripe::verify_webhook(&cfg(ProviderId::Stripe, secret), &hdrs, body)
            .unwrap_err();
    assert_eq!(err, mint_payments::WebhookError::BadSignature);
}

#[test]
fn stripe_wrong_timestamp_is_rejected() {
    let secret = "whsec_stripe_test";
    let body = br#"{"type":"payment_intent.succeeded","data":{"object":{"id":"pi_1","amount_received":100,"metadata":{"account_id":"a"}}}}"#;
    // Sign for one timestamp but present a different `t` in the header.
    let good = stripe_sig(secret.as_bytes(), "1700000000", body);
    let tag = good.split("v1=").nth(1).unwrap();
    let tampered = format!("t=1700009999,v1={tag}");
    let hdrs = common::headers(&[("stripe-signature", &tampered)]);
    assert_eq!(
        mint_payments::card::stripe::verify_webhook(&cfg(ProviderId::Stripe, secret), &hdrs, body),
        Err(mint_payments::WebhookError::BadSignature)
    );
}

#[test]
fn stripe_non_succeeded_event_is_ignored() {
    let secret = "whsec_stripe_test";
    let body = br#"{"type":"payment_intent.created","data":{"object":{"id":"pi_1"}}}"#;
    let hdrs = common::headers(&[(
        "stripe-signature",
        &stripe_sig(secret.as_bytes(), "1", body),
    )]);
    let err =
        mint_payments::card::stripe::verify_webhook(&cfg(ProviderId::Stripe, secret), &hdrs, body)
            .unwrap_err();
    assert!(matches!(err, mint_payments::WebhookError::Ignored(_)));
}

#[tokio::test]
async fn stripe_confirm_deposit_maps_http_statuses() {
    // 2xx confirmed.
    let ok = FakeTransport::new()
        .route(
            "payment_intents",
            200,
            serde_json::json!({"status":"succeeded","amount_received":2299}),
        )
        .arc();
    let ramp = StripeRamp::new(ok, cfg(ProviderId::Stripe, "s"));
    let dep = ramp.confirm_deposit("pi_1").await.expect("confirmed");
    assert_eq!(dep.amount_micros, Micros::new(22_990_000));
    assert!(dep.confirmed);

    // 404 → DepositNotFound.
    let nf = FakeTransport::new()
        .route_status("payment_intents", 404)
        .arc();
    let ramp = StripeRamp::new(nf, cfg(ProviderId::Stripe, "s"));
    assert_eq!(
        ramp.confirm_deposit("pi_x").await,
        Err(RampError::DepositNotFound)
    );

    // 5xx / outage → ProviderUnavailable.
    let ramp = StripeRamp::new(FakeTransport::failing(), cfg(ProviderId::Stripe, "s"));
    assert_eq!(
        ramp.confirm_deposit("pi_x").await,
        Err(RampError::ProviderUnavailable)
    );
}

#[tokio::test]
async fn stripe_payout_pays_and_surfaces_outage() {
    let t = FakeTransport::new()
        .route("transfers", 200, serde_json::json!({"id":"tr_1"}))
        .arc();
    let ramp = StripeRamp::new(t, cfg(ProviderId::Stripe, "s"));
    let dest = PayoutDest::Stripe {
        account: "acct_dest".into(),
    };
    let payout = ramp
        .initiate_payout("stripe:pr_1", &dest, Micros::new(5_000_000))
        .await
        .expect("paid");
    assert_eq!(payout.status, mint_payments::PayoutStatus::Paid);

    let ramp = StripeRamp::new(FakeTransport::failing(), cfg(ProviderId::Stripe, "s"));
    assert_eq!(
        ramp.initiate_payout("stripe:pr_1", &dest, Micros::new(5_000_000))
            .await,
        Err(RampError::ProviderUnavailable)
    );
}

#[test]
fn creem_signed_body_yields_deposit_event() {
    let secret = "creem_secret";
    let body = br#"{"eventType":"checkout.completed","object":{"id":"ch_9","amount":1500,"metadata":{"account_id":"acct_2"}}}"#;
    let hdrs = common::headers(&[("creem-signature", &hmac_hex(secret.as_bytes(), body))]);
    let ev =
        mint_payments::card::creem::verify_webhook(&cfg(ProviderId::Creem, secret), &hdrs, body)
            .expect("verifies");
    assert_eq!(ev.deposit_ref, "ch_9");
    assert_eq!(ev.amount_micros, 15_000_000);
    assert_eq!(ev.account_id, "acct_2");
}

#[tokio::test]
async fn creem_is_billing_only_no_payout() {
    let ramp = CreemRamp::new(FakeTransport::new().arc(), cfg(ProviderId::Creem, "s"));
    let dest = PayoutDest::Stripe {
        account: "x".into(),
    };
    assert_eq!(
        ramp.initiate_payout("creem:1", &dest, Micros::new(1)).await,
        Err(RampError::ProviderUnavailable)
    );
}

#[test]
fn polar_standard_webhooks_yields_deposit_event() {
    let raw_key = b"polar-key";
    let body = br#"{"type":"order.paid","data":{"id":"ord_5","amount":999,"metadata":{"account_id":"acct_3"}}}"#;
    let (secret, sig) = standard_webhooks(raw_key, "msg_1", "1700000000", body);
    let hdrs = common::headers(&[
        ("webhook-id", "msg_1"),
        ("webhook-timestamp", "1700000000"),
        ("webhook-signature", &sig),
    ]);
    let ev =
        mint_payments::card::polar::verify_webhook(&cfg(ProviderId::Polar, &secret), &hdrs, body)
            .expect("verifies");
    assert_eq!(ev.deposit_ref, "ord_5");
    assert_eq!(ev.amount_micros, 9_990_000);
    assert_eq!(ev.account_id, "acct_3");
}

#[test]
fn polar_wrong_timestamp_is_rejected() {
    let raw_key = b"polar-key";
    let body = br#"{"type":"order.paid","data":{"id":"ord_5","amount":999,"metadata":{"account_id":"a"}}}"#;
    let (secret, sig) = standard_webhooks(raw_key, "msg_1", "1700000000", body);
    let hdrs = common::headers(&[
        ("webhook-id", "msg_1"),
        ("webhook-timestamp", "1700009999"), // wrong ts
        ("webhook-signature", &sig),
    ]);
    assert_eq!(
        mint_payments::card::polar::verify_webhook(&cfg(ProviderId::Polar, &secret), &hdrs, body),
        Err(mint_payments::WebhookError::BadSignature)
    );
}

#[tokio::test]
async fn paypal_server_side_verify_yields_deposit_event() {
    use mint_payments::card::PayPalRamp;
    let mut c = cfg(ProviderId::PayPal, "WH-1");
    c.extra.insert("client_secret".into(), "sekret".into());
    // Two HTTP calls: OAuth token, then verify-webhook-signature = SUCCESS.
    let transport = FakeTransport::new()
        .route(
            "oauth2/token",
            200,
            serde_json::json!({"access_token":"tok"}),
        )
        .route(
            "verify-webhook-signature",
            200,
            serde_json::json!({"verification_status":"SUCCESS"}),
        )
        .arc();
    let ramp = PayPalRamp::new(transport, c);
    let body = br#"{"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{"id":"cap_1","custom_id":"acct_4","amount":{"value":"12.50","currency_code":"USD"},"supplementary_data":{"related_ids":{"order_id":"ORDER_1"}}}}"#;
    let hdrs = common::headers(&[
        ("paypal-transmission-id", "tid"),
        ("paypal-transmission-time", "t"),
        ("paypal-cert-url", "url"),
        ("paypal-auth-algo", "SHA256withRSA"),
        ("paypal-transmission-sig", "sig"),
    ]);
    let ev = ramp.verify_webhook(&hdrs, body).await.expect("verifies");
    // Minted under the ORDER id so checkout, capture-return and webhook share
    // one idempotency key.
    assert_eq!(ev.deposit_ref, "ORDER_1");
    assert_eq!(ev.amount_micros, 12_500_000);
    assert_eq!(ev.account_id, "acct_4");
}

#[tokio::test]
async fn paypal_verify_failure_is_bad_signature() {
    use mint_payments::card::PayPalRamp;
    let mut c = cfg(ProviderId::PayPal, "WH-1");
    c.extra.insert("client_secret".into(), "sekret".into());
    let transport = FakeTransport::new()
        .route(
            "oauth2/token",
            200,
            serde_json::json!({"access_token":"tok"}),
        )
        .route(
            "verify-webhook-signature",
            200,
            serde_json::json!({"verification_status":"FAILURE"}),
        )
        .arc();
    let ramp = PayPalRamp::new(transport, c);
    let body = br#"{"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{"id":"cap_1","custom_id":"a","amount":{"value":"1.00"}}}"#;
    let hdrs = common::headers(&[("paypal-transmission-id", "tid")]);
    assert_eq!(
        ramp.verify_webhook(&hdrs, body).await,
        Err(mint_payments::WebhookError::BadSignature)
    );
}

#[test]
fn unconfigured_provider_is_not_configured() {
    let mut c = cfg(ProviderId::Stripe, "");
    c.webhook_secret = None;
    let body = br#"{"type":"payment_intent.succeeded"}"#;
    let hdrs: HashMap<String, String> = HashMap::new();
    assert_eq!(
        mint_payments::card::stripe::verify_webhook(&c, &hdrs, body),
        Err(mint_payments::WebhookError::NotConfigured)
    );
}

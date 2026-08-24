//! Crypto + Turkish group suite: USDC (Coinbase Commerce HMAC + JSON-RPC
//! confirm), Iyzico (3DS callback parse + IYZWSv2 /payment/detail re-query).

mod common;

use std::collections::HashMap;

use common::{hmac_hex, FakeTransport};
use mint_core::Micros;
use mint_payments::config::{Mode, ProviderConfig};
use mint_payments::crypto_tr::{IyzicoRamp, UsdcRamp};
use mint_payments::{Asset, PayoutDest, ProviderId, Ramp, RampError, WebhookError};

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
fn usdc_coinbase_signed_charge_yields_usdc_deposit() {
    let secret = "cc-shared-secret";
    let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"chg_1","pricing":{"local":{"amount":"25.00"}},"metadata":{"account_id":"acct_x"}}}}"#;
    let hdrs = common::headers(&[("x-cc-webhook-signature", &hmac_hex(secret.as_bytes(), body))]);
    let ev =
        mint_payments::crypto_tr::usdc::verify_webhook(&cfg(ProviderId::Usdc, secret), &hdrs, body)
            .expect("verifies");
    assert_eq!(ev.deposit_ref, "chg_1");
    assert_eq!(ev.amount_micros, 25_000_000);
    assert_eq!(ev.asset, Asset::Usdc);
    assert_eq!(ev.account_id, "acct_x");
}

#[test]
fn usdc_bad_signature_is_rejected() {
    let secret = "cc-shared-secret";
    let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"c","pricing":{"local":{"amount":"1.00"}},"metadata":{"account_id":"a"}}}}"#;
    let mut sig = hmac_hex(secret.as_bytes(), body);
    sig.pop();
    sig.push('f');
    let hdrs = common::headers(&[("x-cc-webhook-signature", &sig)]);
    assert_eq!(
        mint_payments::crypto_tr::usdc::verify_webhook(&cfg(ProviderId::Usdc, secret), &hdrs, body),
        Err(WebhookError::BadSignature)
    );
}

#[tokio::test]
async fn usdc_confirm_deposit_reads_receipt_status() {
    let mut c = cfg(ProviderId::Usdc, "s");
    c.extra.insert("rpc_url".into(), "https://rpc.local".into());
    // Confirmed receipt.
    let ok = FakeTransport::new()
        .route(
            "rpc.local",
            200,
            serde_json::json!({"result":{"status":"0x1"}}),
        )
        .arc();
    let ramp = UsdcRamp::new(ok, c.clone());
    let dep = ramp.confirm_deposit("0xabc").await.expect("confirmed");
    assert!(dep.confirmed);
    assert_eq!(dep.asset, Asset::Usdc);

    // Missing tx (result null) → DepositNotFound.
    let nf = FakeTransport::new()
        .route("rpc.local", 200, serde_json::json!({"result": null}))
        .arc();
    let ramp = UsdcRamp::new(nf, c);
    assert_eq!(
        ramp.confirm_deposit("0xdead").await,
        Err(RampError::DepositNotFound)
    );
}

#[test]
fn iyzico_callback_parses_payment_id() {
    let mut c = cfg(ProviderId::Iyzico, "iyz-wh");
    c.extra.insert("secret_key".into(), "sk".into());
    let body = b"status=success&paymentId=pay_42&conversationId=acct_tr&paidPrice=30.00";
    let hdrs: HashMap<String, String> = HashMap::new();
    let ev = mint_payments::crypto_tr::iyzico::verify_webhook(&c, &hdrs, body).expect("parsed");
    assert_eq!(ev.provider, ProviderId::Iyzico);
    assert_eq!(ev.deposit_ref, "pay_42");
    assert_eq!(ev.account_id, "acct_tr");
    assert_eq!(ev.amount_micros, 30_000_000);
    assert_eq!(ev.asset, Asset::Usd);
}

#[tokio::test]
async fn iyzico_confirm_reverifies_via_payment_detail() {
    let mut c = cfg(ProviderId::Iyzico, "iyz-wh");
    c.extra.insert("secret_key".into(), "sk".into());
    let transport = FakeTransport::new()
        .route(
            "/payment/detail",
            200,
            serde_json::json!({"status":"success","paymentStatus":"SUCCESS","paidPrice":"30.00"}),
        )
        .arc();
    let ramp = IyzicoRamp::new(transport, c.clone());
    let dep = ramp.confirm_deposit("pay_42").await.expect("confirmed");
    assert!(dep.confirmed);
    assert_eq!(dep.amount_micros, Micros::new(30_000_000));

    // Outage → ProviderUnavailable.
    let ramp = IyzicoRamp::new(FakeTransport::failing(), c);
    assert_eq!(
        ramp.confirm_deposit("pay_42").await,
        Err(RampError::ProviderUnavailable)
    );
}

#[tokio::test]
async fn iyzico_refund_payout_pays() {
    let mut c = cfg(ProviderId::Iyzico, "iyz-wh");
    c.extra.insert("secret_key".into(), "sk".into());
    let transport = FakeTransport::new()
        .route(
            "/payment/refund",
            200,
            serde_json::json!({"status":"success"}),
        )
        .arc();
    let ramp = IyzicoRamp::new(transport, c);
    let dest = PayoutDest::Stripe {
        account: "n/a".into(),
    };
    let payout = ramp
        .initiate_payout("iyzico:pay_42", &dest, Micros::new(10_000_000))
        .await
        .expect("paid");
    assert_eq!(payout.status, mint_payments::PayoutStatus::Paid);
}

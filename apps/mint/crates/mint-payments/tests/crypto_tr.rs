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
    c.extra.insert("token_contract".into(), "0xtoken".into());
    c.extra.insert("deposit_address".into(), "0x2222222222222222222222222222222222222222".into());
    let tx = "0x00000000000000000000000000000000000000000000000000000000000000ab";
    // Confirmed receipt with a 5 USDC Transfer to the treasury.
    let ok = FakeTransport::new()
        .route(
            "rpc.local",
            200,
            serde_json::json!({"result":{"status":"0x1","logs":[{
                "address":"0xtoken",
                "topics":[
                    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                    "0x0000000000000000000000001111111111111111111111111111111111111111",
                    "0x0000000000000000000000002222222222222222222222222222222222222222"
                ],
                "data":"0x00000000000000000000000000000000000000000000000000000000004c4b40"
            }]}}),
        )
        .arc();
    let ramp = UsdcRamp::new(ok, c.clone());
    let dep = ramp.confirm_deposit(tx).await.expect("confirmed");
    assert!(dep.confirmed);
    assert_eq!(dep.asset, Asset::Usdc);
    assert_eq!(dep.amount_micros, Micros::new(5_000_000));

    // Missing tx (result null) → DepositNotFound.
    let nf = FakeTransport::new()
        .route("rpc.local", 200, serde_json::json!({"result": null}))
        .arc();
    let ramp = UsdcRamp::new(nf, c);
    assert_eq!(
        ramp.confirm_deposit(tx).await,
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
            serde_json::json!({"status":"success","paymentStatus":"SUCCESS","paymentId":"pay_42","currency":"USD","paidPrice":"30.00","basketId":"acct_tr"}),
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
            "/payment/detail",
            200,
            serde_json::json!({"status":"success","currency":"USD","itemTransactions":[{"paymentTransactionId":"7771","paidPrice":"30.00"}]}),
        )
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
        .initiate_payout("12345", &dest, Micros::new(10_000_000))
        .await
        .expect("paid");
    assert_eq!(payout.status, mint_payments::PayoutStatus::Paid);
}

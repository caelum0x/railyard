//! Phase 6 proofs for the payment + webhook adapters, verified against the
//! in-memory transport (no network). Numbers/shapes derived from the Lago Ruby.

use rago_integrations::payments::stripe::StripeProvider;
use rago_integrations::payments::{PayableStatus, PaymentError, PaymentProvider, PaymentRequest};
use rago_integrations::transport::MockTransport;
use rago_integrations::webhooks::{deliver, Webhook, WebhookStatus};
use serde_json::json;
use uuid::Uuid;

fn payment_request() -> PaymentRequest {
    PaymentRequest {
        payment_id: Uuid::from_u128(1),
        invoice_id: Uuid::from_u128(2),
        customer_id: Uuid::from_u128(3),
        provider_customer_id: "cus_ABC".into(),
        amount_cents: 36000,
        currency: "USD".into(),
        reference: "INV-001".into(),
        provider_mandate_id: None,
    }
}

#[tokio::test]
async fn stripe_payment_intent_shape_and_success() {
    let transport = MockTransport::always(200, r#"{"id":"pi_123","status":"succeeded"}"#);
    let provider = StripeProvider::new("sk_test_x", transport);
    let req = payment_request();

    let outcome = provider.create_payment(&req).await.unwrap();
    assert_eq!(outcome.provider_payment_id, "pi_123");
    assert_eq!(outcome.provider_status, "succeeded");
    assert_eq!(outcome.payable_status, PayableStatus::Succeeded);
}

#[tokio::test]
async fn stripe_request_headers_and_body() {
    // Inspect the recorded request via a transport we keep a handle to.
    use rago_integrations::transport::HttpTransport;
    let transport = MockTransport::always(200, r#"{"id":"pi_9","status":"succeeded"}"#);
    // Drive the transport directly through the provider by cloning intent building:
    let req = payment_request();
    // Build the provider borrowing the transport by reference wrapper.
    struct Ref<'a>(&'a MockTransport);
    #[async_trait::async_trait]
    impl HttpTransport for Ref<'_> {
        async fn post(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: String,
        ) -> Result<rago_integrations::transport::HttpResponse, rago_integrations::transport::TransportError>
        {
            self.0.post(url, headers, body).await
        }
    }
    let provider = StripeProvider::new("sk_test_abc", Ref(&transport));
    provider.create_payment(&req).await.unwrap();

    let rr = transport.last_request().unwrap();
    assert_eq!(rr.url, "https://api.stripe.com/v1/payment_intents");
    assert!(rr.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer sk_test_abc"));
    assert!(rr.headers.iter().any(|(k, v)| k == "Idempotency-Key" && v == "payment-00000000-0000-0000-0000-000000000001"));
    assert!(rr.body.contains("amount=36000"));
    assert!(rr.body.contains("currency=usd"));
    assert!(rr.body.contains("confirm=true"));
    assert!(rr.body.contains("metadata%5Blago_payable_type%5D=Invoice"));
}

#[test]
fn stripe_status_mapping() {
    use StripeProvider as S;
    type T = MockTransport;
    assert_eq!(S::<T>::determine_payment_status("succeeded"), PayableStatus::Succeeded);
    assert_eq!(S::<T>::determine_payment_status("canceled"), PayableStatus::Failed);
    assert_eq!(S::<T>::determine_payment_status("requires_payment_method"), PayableStatus::Failed);
    assert_eq!(S::<T>::determine_payment_status("processing"), PayableStatus::Pending);
    assert_eq!(S::<T>::determine_payment_status("requires_action"), PayableStatus::Pending);
}

#[tokio::test]
async fn stripe_error_response() {
    let transport = MockTransport::always(402, r#"{"error":{"code":"card_declined","message":"Your card was declined."}}"#);
    let provider = StripeProvider::new("sk_test_x", transport);
    // A card decline is a recorded business outcome, not a transport error.
    let outcome = provider.create_payment(&payment_request()).await.unwrap();
    assert_eq!(outcome.payable_status, PayableStatus::Failed);
    assert_eq!(outcome.provider_status, "card_declined");
}

// --- webhooks ---------------------------------------------------------------

fn webhook() -> Webhook {
    Webhook::build(
        Uuid::from_u128(10),
        "https://example.test/hooks",
        "invoice.created",
        "invoice",
        json!({ "lago_id": "inv_1", "total_amount_cents": 36000 }),
    )
}

#[test]
fn webhook_payload_and_signature() {
    let w = webhook();
    // Payload shape: { webhook_type, object_type, <object_type>: {...} }.
    assert_eq!(w.payload["webhook_type"], "invoice.created");
    assert_eq!(w.payload["object_type"], "invoice");
    assert_eq!(w.payload["invoice"]["total_amount_cents"], 36000);

    // HMAC signature is deterministic and key-dependent.
    let sig_a = w.hmac_signature("key-a");
    let sig_a2 = w.hmac_signature("key-a");
    let sig_b = w.hmac_signature("key-b");
    assert_eq!(sig_a, sig_a2);
    assert_ne!(sig_a, sig_b);
    assert!(!sig_a.is_empty());

    let headers = w.headers("key-a");
    assert!(headers.iter().any(|(k, _)| k == "X-Lago-Signature"));
    assert!(headers.iter().any(|(k, v)| k == "X-Lago-Signature-Algorithm" && v == "hmac"));
    assert!(headers.iter().any(|(k, v)| k == "X-Lago-Unique-Key" && v == &w.id.to_string()));
}

#[tokio::test]
async fn webhook_delivery_success() {
    let transport = MockTransport::always(200, "{}");
    let mut w = webhook();
    let retry = deliver(&transport, &mut w, "hmac-key").await;
    assert!(!retry);
    assert_eq!(w.status, WebhookStatus::Succeeded);
    assert_eq!(w.http_status, Some(200));

    // The signed request was actually sent with the headers.
    let rr = transport.last_request().unwrap();
    assert_eq!(rr.url, "https://example.test/hooks");
    assert!(rr.headers.iter().any(|(k, _)| k == "X-Lago-Signature"));
}

#[tokio::test]
async fn webhook_delivery_retries_then_fails() {
    let transport = MockTransport::always(500, "server error");
    let mut w = webhook();

    // Attempt 1 → retrying.
    assert!(deliver(&transport, &mut w, "k").await);
    assert_eq!(w.status, WebhookStatus::Retrying);
    assert_eq!(w.retries, 1);
    assert_eq!(w.backoff_seconds(), 3.0); // 1^4 + 2

    // Attempt 2 → retrying.
    assert!(deliver(&transport, &mut w, "k").await);
    assert_eq!(w.status, WebhookStatus::Retrying);
    assert_eq!(w.retries, 2);
    assert_eq!(w.backoff_seconds(), 18.0); // 2^4 + 2

    // Attempt 3 → failed (retry limit reached).
    assert!(!deliver(&transport, &mut w, "k").await);
    assert_eq!(w.status, WebhookStatus::Failed);
    assert_eq!(w.retries, 3);
}

// --- incoming Stripe webhooks ----------------------------------------------

use rago_integrations::payments::stripe_webhook::{
    parse_event, signature_header, verify_signature, IncomingStatus, WebhookError,
};

#[test]
fn stripe_webhook_signature_verify() {
    let secret = "whsec_test";
    let payload = r#"{"type":"payment_intent.succeeded","data":{"object":{"id":"pi_1","status":"succeeded"}}}"#;
    let now = 1_700_000_000i64;
    let header = signature_header(secret, now, payload);

    // Valid signature within tolerance.
    assert!(verify_signature(payload, &header, secret, now, 300).is_ok());
    // Wrong secret.
    assert_eq!(
        verify_signature(payload, &header, "whsec_other", now, 300),
        Err(WebhookError::SignatureMismatch)
    );
    // Tampered payload.
    assert_eq!(
        verify_signature(r#"{"tampered":true}"#, &header, secret, now, 300),
        Err(WebhookError::SignatureMismatch)
    );
    // Outside tolerance.
    assert_eq!(
        verify_signature(payload, &header, secret, now + 1000, 300),
        Err(WebhookError::TimestampOutOfTolerance)
    );
}

#[test]
fn stripe_webhook_event_parse_and_mapping() {
    let payload = r#"{"type":"payment_intent.succeeded","data":{"object":{"id":"pi_ABC","status":"succeeded","metadata":{"lago_payment_id":"p1","lago_payable_id":"inv1"}}}}"#;
    let ev = parse_event(payload).unwrap();
    assert_eq!(ev.event_type, "payment_intent.succeeded");
    assert_eq!(ev.object_id, "pi_ABC");
    assert_eq!(ev.incoming_status(), IncomingStatus::Succeeded);
    assert_eq!(ev.lago_payment_id(), Some("p1"));

    let failed = parse_event(r#"{"type":"payment_intent.payment_failed","data":{"object":{"id":"pi_2"}}}"#).unwrap();
    assert_eq!(failed.incoming_status(), IncomingStatus::Failed);

    let refunded = parse_event(r#"{"type":"charge.refunded","data":{"object":{"id":"ch_1"}}}"#).unwrap();
    assert_eq!(refunded.incoming_status(), IncomingStatus::Refunded);

    let other = parse_event(r#"{"type":"customer.updated","data":{"object":{"id":"cus_1"}}}"#).unwrap();
    assert_eq!(other.incoming_status(), IncomingStatus::Ignored);
}

// --- GoCardless + Adyen providers ------------------------------------------

use rago_integrations::payments::adyen::AdyenProvider;
use rago_integrations::payments::gocardless::GocardlessProvider;
use rago_integrations::payments::provider_webhook::{
    adyen_event, adyen_expected_signature, adyen_verify, gocardless_parse, gocardless_verify,
};

fn mandate_request() -> PaymentRequest {
    let mut r = payment_request();
    r.provider_mandate_id = Some("MD123".into());
    r
}

#[tokio::test]
async fn gocardless_create_payment_and_status() {
    let transport = MockTransport::always(200, r#"{"payments":{"id":"PM01","status":"submitted"}}"#);
    let provider = GocardlessProvider::new("gc_token", transport);
    let outcome = provider.create_payment(&mandate_request()).await.unwrap();
    assert_eq!(outcome.provider_payment_id, "PM01");
    assert_eq!(outcome.provider_status, "submitted");
    assert_eq!(outcome.payable_status, PayableStatus::Pending);

    type G = GocardlessProvider<MockTransport>;
    assert_eq!(G::determine_payment_status("paid_out"), PayableStatus::Succeeded);
    assert_eq!(G::determine_payment_status("failed"), PayableStatus::Failed);
    assert_eq!(G::determine_payment_status("charged_back"), PayableStatus::Failed);
    assert_eq!(G::determine_payment_status("submitted"), PayableStatus::Pending);
}

#[tokio::test]
async fn gocardless_requires_mandate() {
    let transport = MockTransport::always(200, "{}");
    let provider = GocardlessProvider::new("gc_token", transport);
    let err = provider.create_payment(&payment_request()).await.unwrap_err();
    match err {
        PaymentError::Provider { code, .. } => assert_eq!(code.as_deref(), Some("no_mandate_error")),
        other => panic!("expected mandate error, got {other:?}"),
    }
}

#[tokio::test]
async fn gocardless_request_shape() {
    use rago_integrations::transport::HttpTransport;
    let transport = MockTransport::always(200, r#"{"payments":{"id":"PM1","status":"paid_out"}}"#);
    struct Ref<'a>(&'a MockTransport);
    #[async_trait::async_trait]
    impl HttpTransport for Ref<'_> {
        async fn post(&self, u: &str, h: &[(String, String)], b: String)
            -> Result<rago_integrations::transport::HttpResponse, rago_integrations::transport::TransportError> {
            self.0.post(u, h, b).await
        }
    }
    let provider = GocardlessProvider::new("gc_secret", Ref(&transport));
    provider.create_payment(&mandate_request()).await.unwrap();
    let rr = transport.last_request().unwrap();
    assert_eq!(rr.url, "https://api.gocardless.com/payments");
    assert!(rr.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer gc_secret"));
    assert!(rr.headers.iter().any(|(k, _)| k == "GoCardless-Version"));
    assert!(rr.body.contains("\"mandate\":\"MD123\""));
    assert!(rr.body.contains("\"amount\":36000"));
}

#[tokio::test]
async fn adyen_create_payment_and_status() {
    let transport = MockTransport::always(200, r#"{"resultCode":"Authorised","pspReference":"PSP42"}"#);
    let provider = AdyenProvider::new("adyen_key", "MerchantX", transport);
    let outcome = provider.create_payment(&payment_request()).await.unwrap();
    assert_eq!(outcome.provider_payment_id, "PSP42");
    assert_eq!(outcome.provider_status, "Authorised");
    assert_eq!(outcome.payable_status, PayableStatus::Succeeded);

    type A = AdyenProvider<MockTransport>;
    assert_eq!(A::determine_payment_status("Received"), PayableStatus::Pending);
    assert_eq!(A::determine_payment_status("Settled"), PayableStatus::Succeeded);
    assert_eq!(A::determine_payment_status("Refused"), PayableStatus::Failed);
}

#[test]
fn gocardless_webhook_verify_and_parse() {
    let secret = "gc_wh_secret";
    let body = r#"{"events":[{"resource_type":"payments","action":"paid_out","links":{"payment":"PM99"}}]}"#;
    // Sign with the same HMAC the verifier expects.
    let expected = {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    assert!(gocardless_verify(body, &expected, secret).is_ok());
    assert!(gocardless_verify(body, "bad", secret).is_err());

    let events = gocardless_parse(body).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].provider_payment_id, "PM99");
    assert_eq!(events[0].status, IncomingStatus::Succeeded);
}

#[test]
fn adyen_webhook_verify_and_map() {
    let secret_hex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef01";
    let mut item = serde_json::json!({
        "pspReference": "PSP7", "merchantAccountCode": "MerchantX", "merchantReference": "INV-1",
        "amount": {"value": 36000, "currency": "USD"}, "eventCode": "AUTHORISATION", "success": "true"
    });
    let sig = adyen_expected_signature(&item, secret_hex).unwrap();
    item["additionalData"] = serde_json::json!({ "hmacSignature": sig });
    assert!(adyen_verify(&item, secret_hex).is_ok());

    let ev = adyen_event(&item);
    assert_eq!(ev.provider_payment_id, "PSP7");
    assert_eq!(ev.status, IncomingStatus::Succeeded);
}

// --- integrations (tax / accounting / crm) ---------------------------------

use rago_integrations::{
    AccountingInvoice, AccountingProvider, AnrokTaxProvider, CrmCustomer, CrmProvider,
    HubspotProvider, NetsuiteProvider, TaxFeeItem, TaxProvider, TaxRequest,
};

#[tokio::test]
async fn anrok_tax_fetch_and_breakdown() {
    // Provider returns 8.5% on a 5000c fee → 425c, with a breakdown line.
    let body = r#"{"fees":[{"item_id":"fee-1","tax_amount_cents":425,"tax_breakdown":[{"name":"CA State Tax","rate":0.085,"tax_amount":425,"type":"sales_tax"}]}]}"#;
    let provider = AnrokTaxProvider::new("https://agg.example", "anrok_key", MockTransport::always(200, body));
    let req = TaxRequest {
        invoice_id: "inv-1".into(),
        currency: "USD".into(),
        customer_external_id: "cust-1".into(),
        fees: vec![TaxFeeItem { item_id: "fee-1".into(), item_code: "api".into(), amount_cents: 5000 }],
    };
    let results = provider.fetch_invoice_taxes(&req).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item_id, "fee-1");
    assert_eq!(results[0].tax_amount_cents, 425);
    assert_eq!(results[0].tax_breakdown[0].name, "CA State Tax");
    assert!((results[0].tax_breakdown[0].rate - 0.085).abs() < 1e-9);
}

#[tokio::test]
async fn anrok_request_shape() {
    use rago_integrations::transport::HttpTransport;
    let transport = MockTransport::always(200, r#"{"fees":[]}"#);
    struct Ref<'a>(&'a MockTransport);
    #[async_trait::async_trait]
    impl HttpTransport for Ref<'_> {
        async fn post(&self, u: &str, h: &[(String, String)], b: String)
            -> Result<rago_integrations::transport::HttpResponse, rago_integrations::transport::TransportError> {
            self.0.post(u, h, b).await
        }
    }
    let provider = AnrokTaxProvider::new("https://agg.example", "anrok_secret", Ref(&transport));
    let req = TaxRequest { invoice_id: "inv-9".into(), currency: "USD".into(), customer_external_id: "c9".into(),
        fees: vec![TaxFeeItem { item_id: "f9".into(), item_code: "api".into(), amount_cents: 1000 }] };
    provider.fetch_invoice_taxes(&req).await.unwrap();
    let rr = transport.last_request().unwrap();
    assert_eq!(rr.url, "https://agg.example/v1/anrok/finalized_invoices");
    assert!(rr.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer anrok_secret"));
    assert!(rr.body.contains("\"item_id\":\"f9\""));
}

#[tokio::test]
async fn netsuite_and_hubspot_sync() {
    let ns = NetsuiteProvider::new("https://agg.example", "ns_key", MockTransport::always(200, r#"{"succeededId":"NS-100"}"#));
    let sync = ns.sync_invoice(&AccountingInvoice {
        invoice_id: "inv-1".into(), number: "INV-1".into(), currency: "USD".into(),
        total_amount_cents: 36000, customer_external_id: "cust-1".into(),
        ..Default::default()
    }).await.unwrap();
    assert_eq!(sync.external_id, "NS-100");
    assert_eq!(ns.provider_type(), "netsuite");

    let hs = HubspotProvider::new("https://agg.example", "hs_key", MockTransport::always(200, r#"{"id":"HS-7"}"#));
    let sync = hs.sync_customer(&CrmCustomer {
        customer_id: "c1".into(), external_id: "cust-1".into(),
        name: Some("Acme".into()), email: Some("a@acme.test".into()),
    }).await.unwrap();
    assert_eq!(sync.external_id, "HS-7");
    assert_eq!(hs.provider_type(), "hubspot");
}

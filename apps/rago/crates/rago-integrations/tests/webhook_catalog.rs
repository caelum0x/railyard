//! Parity proofs for the full webhook catalog: every [`WebhookKind`] maps to the
//! correct Lago `webhook_type`/`object_type`, the envelope is shaped like
//! `Webhooks::BaseService#call`, the bespoke payload serializers match their
//! `V1::…Serializer`, and a representative kind round-trips through the signed
//! delivery infra. Shapes derived from Lago Ruby serializers (do not fabricate).

use rago_integrations::transport::MockTransport;
use rago_integrations::webhooks::{deliver, payloads, WebhookKind};
use rago_integrations::WebhookStatus;
use serde_json::json;
use uuid::Uuid;

const ORG: Uuid = Uuid::from_u128(0x42);

// --- catalog integrity ------------------------------------------------------

#[test]
fn catalog_has_full_registry_and_unique_types() {
    // 75 registry entries (Lago SendWebhookJob::WEBHOOK_SERVICES).
    assert_eq!(WebhookKind::ALL.len(), 75);

    // webhook_type strings are all distinct and dotted.
    let mut types: Vec<&str> = WebhookKind::ALL.iter().map(|k| k.webhook_type()).collect();
    let count = types.len();
    types.sort_unstable();
    types.dedup();
    assert_eq!(types.len(), count, "duplicate webhook_type strings");
    assert!(WebhookKind::ALL.iter().all(|k| k.webhook_type().contains('.')));
}

#[test]
fn from_webhook_type_round_trips_every_kind() {
    for k in WebhookKind::ALL {
        assert_eq!(WebhookKind::from_webhook_type(k.webhook_type()), Some(k));
    }
    assert_eq!(WebhookKind::from_webhook_type("nope.nope"), None);
}

#[test]
fn bespoke_object_types_match_lago() {
    // Spot-check the pairs that differ from the resource name.
    assert_eq!(
        WebhookKind::InvoicePaymentFailure.object_type(),
        "payment_provider_invoice_payment_error"
    );
    assert_eq!(
        WebhookKind::InvoicePaymentDisputeLost.object_type(),
        "payment_dispute_lost"
    );
    assert_eq!(WebhookKind::AlertTriggered.object_type(), "triggered_alert");
    assert_eq!(
        WebhookKind::CustomerCheckoutUrlGenerated.object_type(),
        "payment_provider_customer_checkout_url"
    );
    assert_eq!(
        WebhookKind::CreditNoteRefundFailure.webhook_type(),
        "credit_note.refund_failure"
    );
    // Plain model kinds nest under the resource.
    assert_eq!(WebhookKind::InvoiceCreated.object_type(), "invoice");
    assert_eq!(WebhookKind::SubscriptionUpdated.object_type(), "subscription");
}

#[test]
fn schema_deferred_flags_orders_and_receipts_only() {
    let deferred: Vec<&str> = WebhookKind::ALL
        .iter()
        .filter(|k| k.is_schema_deferred())
        .map(|k| k.webhook_type())
        .collect();
    assert_eq!(
        deferred,
        vec![
            "order.created",
            "order.executed",
            "order_form.created",
            "order_form.signed",
            "order_form.expired",
            "order_form.voided",
            "payment_receipt.created",
            "payment_receipt.generated",
        ]
    );
}

// --- envelope (Webhooks::BaseService#call) ----------------------------------

#[test]
fn build_wraps_object_in_lago_envelope() {
    let object = json!({ "lago_id": "sub_1", "status": "active" });
    let w = WebhookKind::SubscriptionStarted.build(ORG, "https://hooks.test", object.clone());

    assert_eq!(w.payload["webhook_type"], "subscription.started");
    assert_eq!(w.payload["object_type"], "subscription");
    assert_eq!(w.payload["organization_id"], ORG.to_string());
    assert_eq!(w.payload["subscription"], object);
    assert_eq!(w.webhook_type, "subscription.started");
    assert_eq!(w.object_type, "subscription");
}

#[test]
fn build_nests_bespoke_object_under_object_type_key() {
    let object = payloads::invoice_payment_error(
        "inv_1",
        "cus_1",
        "ext_1",
        Some("pi_cus_1"),
        Some("stripe"),
        Some("stripe_1"),
        json!({ "message": "card declined" }),
        json!({ "code": "card_declined" }),
    );
    let w = WebhookKind::InvoicePaymentFailure.build(ORG, "https://hooks.test", object);

    assert_eq!(w.payload["webhook_type"], "invoice.payment_failure");
    assert_eq!(
        w.payload["object_type"],
        "payment_provider_invoice_payment_error"
    );
    let obj = &w.payload["payment_provider_invoice_payment_error"];
    assert_eq!(obj["lago_invoice_id"], "inv_1");
    assert_eq!(obj["provider_customer_id"], "pi_cus_1");
    assert_eq!(obj["payment_provider"], "stripe");
    assert_eq!(obj["provider_error"]["message"], "card declined");
    assert_eq!(obj["error_details"]["code"], "card_declined");
}

/// The provider-settlement path (rago-api incoming webhook) emits
/// `payment.succeeded` on capture and `payment_provider.error` on decline. Prove
/// both envelopes carry the exact catalog type/object_type + payload the handler
/// builds, so the emit at the provider trigger point is on-contract.
#[test]
fn provider_settlement_path_envelopes() {
    // payment.succeeded — a plain object nested under `payment`.
    let paid = json!({
        "lago_id": "pay_1",
        "lago_invoice_id": "inv_1",
        "status": "succeeded",
        "amount_cents": 1000,
        "amount_currency": "USD",
    });
    let w = WebhookKind::PaymentSucceeded.build(ORG, "https://hooks.test", paid);
    assert_eq!(w.payload["webhook_type"], "payment.succeeded");
    assert_eq!(w.payload["object_type"], "payment");
    assert_eq!(w.payload["payment"]["status"], "succeeded");

    // payment_provider.error — the bespoke error serializer, source/action hoisted.
    let object = payloads::payment_provider_error(
        &ORG.to_string(),
        Some("stripe"),
        Some("Stripe"),
        json!({ "source": "stripe", "action": "capture", "message": "card_declined" }),
    );
    let w = WebhookKind::PaymentProviderError.build(ORG, "https://hooks.test", object);
    assert_eq!(w.payload["webhook_type"], "payment_provider.error");
    assert_eq!(w.payload["object_type"], "payment_provider_error");
    let obj = &w.payload["payment_provider_error"];
    assert_eq!(obj["source"], "stripe");
    assert_eq!(obj["action"], "capture");
    assert_eq!(obj["provider_error"]["message"], "card_declined");
}

// --- bespoke payload serializer shapes --------------------------------------

#[test]
fn customer_checkout_url_shape() {
    let v = payloads::customer_checkout_url(
        "cus_1",
        "ext_1",
        Some("stripe"),
        Some("stripe_1"),
        "https://checkout.test/abc",
    );
    assert_eq!(
        v,
        json!({
            "lago_customer_id": "cus_1",
            "external_customer_id": "ext_1",
            "payment_provider": "stripe",
            "payment_provider_code": "stripe_1",
            "checkout_url": "https://checkout.test/abc",
        })
    );
}

#[test]
fn customer_error_shape_and_nil_provider() {
    // payment_provider unset -> null (Ruby nil).
    let v = payloads::payment_provider_customer_error(
        "cus_1",
        "ext_1",
        None,
        None,
        json!({ "message": "no provider" }),
    );
    assert_eq!(
        v,
        json!({
            "lago_customer_id": "cus_1",
            "external_customer_id": "ext_1",
            "payment_provider": null,
            "payment_provider_code": null,
            "provider_error": { "message": "no provider" },
        })
    );
}

#[test]
fn payment_provider_error_hoists_source_and_action() {
    let v = payloads::payment_provider_error(
        "pp_1",
        Some("stripe_eu"),
        Some("Stripe EU"),
        json!({ "source": "stripe", "action": "payment_provider.register", "code": "invalid_key", "message": "bad key" }),
    );
    assert_eq!(v["lago_payment_provider_id"], "pp_1");
    assert_eq!(v["payment_provider_code"], "stripe_eu");
    assert_eq!(v["payment_provider_name"], "Stripe EU");
    assert_eq!(v["source"], "stripe");
    assert_eq!(v["action"], "payment_provider.register");
    // source/action removed from the nested provider_error.
    assert_eq!(v["provider_error"], json!({ "code": "invalid_key", "message": "bad key" }));
}

#[test]
fn payment_request_payment_error_shape() {
    let v = payloads::payment_request_payment_error(
        "pr_1",
        vec!["inv_1".into(), "inv_2".into()],
        "cus_1",
        "ext_1",
        Some("pi_cus"),
        Some("stripe"),
        Some("stripe_1"),
        json!({ "message": "declined" }),
    );
    assert_eq!(v["lago_payment_request_id"], "pr_1");
    assert_eq!(v["lago_invoice_ids"], json!(["inv_1", "inv_2"]));
    assert_eq!(v["provider_customer_id"], "pi_cus");
    assert_eq!(v["provider_error"]["message"], "declined");
}

#[test]
fn tax_fee_error_shape() {
    let v = payloads::tax_fee_error(
        Some("avalara_1"),
        Some("charge_1"),
        Some("evt_txn_1"),
        json!({ "message": "tax error" }),
    );
    assert_eq!(
        v,
        json!({
            "tax_provider_code": "avalara_1",
            "lago_charge_id": "charge_1",
            "event_transaction_id": "evt_txn_1",
            "provider_error": { "message": "tax error" },
        })
    );
}

#[test]
fn integration_provider_error_shape() {
    let v = payloads::integration_provider_error(
        "int_1",
        Some("netsuite"),
        Some("ns_1"),
        json!({ "message": "sync failed" }),
    );
    assert_eq!(v["lago_integration_id"], "int_1");
    assert_eq!(v["provider"], "netsuite");
    assert_eq!(v["provider_code"], "ns_1");
    assert_eq!(v["provider_error"]["message"], "sync failed");
}

#[test]
fn event_error_encodes_message_as_json_string_and_merges_event() {
    let error = json!({ "code": "invalid_json" });
    let event = json!({ "transaction_id": "txn_1", "code": "api_calls" });
    let v = payloads::event_error(&error, event.clone());
    assert_eq!(v["status"], 422);
    assert_eq!(v["error"], "Unprocessable entity");
    // message is the JSON *string* of the error (Ruby error.to_json).
    assert_eq!(v["message"], serde_json::to_string(&error).unwrap());
    assert_eq!(v["message"], "{\"code\":\"invalid_json\"}");
    assert_eq!(v["event"], event);
}

#[test]
fn events_errors_defaults_missing_fields_to_null() {
    let errors = json!({ "invalid_code": ["evt_1"], "missing_group_key": ["evt_2"] });
    let v = payloads::events_errors(&errors);
    assert_eq!(v["invalid_code"], json!(["evt_1"]));
    assert_eq!(v["missing_group_key"], json!(["evt_2"]));
    assert_eq!(v["missing_aggregation_property"], serde_json::Value::Null);
    assert_eq!(v["invalid_filter_values"], serde_json::Value::Null);
}

#[test]
fn payment_dispute_lost_omits_provider_error_when_absent() {
    let invoice = json!({ "lago_id": "inv_1", "customer": { "lago_id": "cus_1" } });
    let without = payloads::payment_dispute_lost(invoice.clone(), None);
    assert_eq!(without, json!({ "invoice": invoice }));

    let with = payloads::payment_dispute_lost(invoice.clone(), Some(json!({ "reason": "lost" })));
    assert_eq!(with["invoice"], invoice);
    assert_eq!(with["provider_error"], json!({ "reason": "lost" }));

    // null provider_error is treated as absent.
    let null_pe = payloads::payment_dispute_lost(invoice, Some(serde_json::Value::Null));
    assert!(null_pe.get("provider_error").is_none());
}

// --- signed delivery round-trip ---------------------------------------------

#[tokio::test]
async fn catalog_webhook_delivers_signed_through_transport() {
    let transport = MockTransport::always(200, "{}");
    let object = payloads::wallet_transaction_payment_error(
        "wt_1",
        "cus_1",
        "ext_1",
        Some("pi_cus"),
        Some("stripe"),
        Some("stripe_1"),
        json!({ "message": "declined" }),
    );
    let mut w =
        WebhookKind::WalletTransactionPaymentFailure.build(ORG, "https://hooks.test/wt", object);

    let retry = deliver(&transport, &mut w, "hmac-key").await;
    assert!(!retry);
    assert_eq!(w.status, WebhookStatus::Succeeded);
    assert_eq!(w.http_status, Some(200));

    let rr = transport.last_request().unwrap();
    assert_eq!(rr.url, "https://hooks.test/wt");
    assert!(rr
        .headers
        .iter()
        .any(|(k, v)| k == "X-Lago-Signature-Algorithm" && v == "hmac"));
    assert!(rr.headers.iter().any(|(k, _)| k == "X-Lago-Signature"));
    // Signature matches a fresh HMAC over the exact delivered body.
    assert_eq!(
        rr.headers
            .iter()
            .find(|(k, _)| k == "X-Lago-Signature")
            .map(|(_, v)| v.clone()),
        Some(w.hmac_signature("hmac-key"))
    );
}

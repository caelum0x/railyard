//! The seller's own Dodo Payments account (Wave P PY2.3). Every call uses the
//! **seller's** API key on the seller's account: products, checkout sessions,
//! and the payment lookup a refund or dispute needs. Webhooks are verified
//! with the **seller's** webhook secret (Standard Webhooks) and mapped to
//! entitlement events. Mint never touches the money.

use serde_json::{json, Value};

use super::{meta_from, CheckoutMeta, CheckoutSession, EntitlementEvent, EntitlementKind, Interval, ProductSpec, RevokeReason, SellerAccount, SellerError, SellerMode};
use crate::http::{Headers, HttpTransport};
use crate::verify::standard_webhooks_verify;
use crate::WebhookError;

/// Seconds either side of now a delivery may be stamped.
pub const TOLERANCE_SECS: i64 = 300;

pub fn api_base(mode: SellerMode) -> &'static str {
    match mode {
        SellerMode::Test => "https://test.dodopayments.com",
        SellerMode::Live => "https://live.dodopayments.com",
    }
}

fn auth(acct: &SellerAccount) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {}", acct.secret)),
        ("content-type".into(), "application/json".into()),
    ]
}

fn answer(status: u16, body: &[u8]) -> Result<Value, SellerError> {
    match status {
        200..=299 => serde_json::from_slice(body).map_err(|_| SellerError::Unavailable),
        401 | 403 => Err(SellerError::Refused("the key was not accepted".into())),
        400..=499 => {
            let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let msg = v["message"].as_str().or_else(|| v["error"].as_str()).unwrap_or("invalid request");
            Err(SellerError::Refused(msg.chars().take(200).collect()))
        }
        _ => Err(SellerError::Unavailable),
    }
}

/// Checks the key works on its account (used when the seller connects).
///
/// # Errors
/// `Refused` for a key the account does not accept.
pub async fn validate(transport: &dyn HttpTransport, acct: &SellerAccount) -> Result<(), SellerError> {
    let url = format!("{}/products?page_size=1", api_base(acct.mode));
    let resp = transport.get(&url, &auth(acct)).await.map_err(|_| SellerError::Unavailable)?;
    answer(resp.status, &resp.body).map(|_| ())
}

/// The body `POST /products` takes for `spec`.
pub fn product_body(spec: &ProductSpec) -> Value {
    let price = match spec.interval {
        None => json!({ "type": "one_time_price", "price": spec.price_cents, "currency": spec.currency,
                        "discount": 0, "purchasing_power_parity": false }),
        Some(interval) => json!({ "type": "recurring_price", "price": spec.price_cents, "currency": spec.currency,
                        "discount": 0, "purchasing_power_parity": false,
                        "payment_frequency_count": 1, "payment_frequency_interval": if interval == Interval::Month { "Month" } else { "Year" },
                        "subscription_period_count": 10, "subscription_period_interval": "Year" }),
    };
    json!({ "name": spec.name, "description": spec.name, "price": price, "tax_category": "saas",
            "metadata": { "sku": spec.sku } })
}

/// Creates the product on the seller's account; returns its product id.
///
/// # Errors
/// Invalid spec, a refusal, or the account not answering.
pub async fn create_product(transport: &dyn HttpTransport, acct: &SellerAccount, spec: &ProductSpec) -> Result<String, SellerError> {
    spec.validate().map_err(SellerError::Invalid)?;
    let url = format!("{}/products", api_base(acct.mode));
    let body = serde_json::to_vec(&product_body(spec)).map_err(|_| SellerError::Unavailable)?;
    let resp = transport.post_json(&url, &auth(acct), &body).await.map_err(|_| SellerError::Unavailable)?;
    let v = answer(resp.status, &resp.body)?;
    v["product_id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned).ok_or(SellerError::Unavailable)
}

/// Opens a hosted checkout on the seller's account for one buyer.
///
/// # Errors
/// A refusal, or the account not answering.
pub async fn create_checkout(
    transport: &dyn HttpTransport,
    acct: &SellerAccount,
    product_id: &str,
    meta: &CheckoutMeta<'_>,
    return_url: &str,
) -> Result<CheckoutSession, SellerError> {
    let body = json!({
        "product_cart": [{ "product_id": product_id, "quantity": 1 }],
        "return_url": return_url,
        "metadata": { "mint_seller": meta.seller_key, "buyer_ref": meta.buyer_ref, "sku": meta.sku },
    });
    let url = format!("{}/checkouts", api_base(acct.mode));
    let raw = serde_json::to_vec(&body).map_err(|_| SellerError::Unavailable)?;
    let resp = transport.post_json(&url, &auth(acct), &raw).await.map_err(|_| SellerError::Unavailable)?;
    let v = answer(resp.status, &resp.body)?;
    match (v["checkout_url"].as_str(), v["session_id"].as_str()) {
        (Some(url), Some(id)) if url.starts_with("https://") && !id.is_empty() => Ok(CheckoutSession { url: url.to_owned(), id: id.to_owned() }),
        _ => Err(SellerError::Unavailable),
    }
}

/// Verifies one delivery from the seller's Dodo account and maps it.
/// `Ok(None)` for an event that changes no entitlement. An event whose
/// metadata names another seller is ignored (it cannot touch this one).
///
/// # Errors
/// `BadSignature` / `NotConfigured`, or `Malformed` for a payload Mint made
/// that no longer parses.
pub async fn verify_event<H: Headers>(
    transport: &dyn HttpTransport,
    acct: &SellerAccount,
    seller_key: &str,
    headers: &H,
    raw: &[u8],
    now_secs: i64,
) -> Result<Option<EntitlementEvent>, WebhookError> {
    let secret = acct.webhook_secret.as_deref().filter(|s| !s.is_empty()).ok_or(WebhookError::NotConfigured)?;
    let id = headers.get_header("webhook-id").ok_or(WebhookError::BadSignature)?;
    let ts = headers.get_header("webhook-timestamp").ok_or(WebhookError::BadSignature)?;
    let sig = headers.get_header("webhook-signature").ok_or(WebhookError::BadSignature)?;
    let ts_num: i64 = ts.trim().parse().map_err(|_| WebhookError::BadSignature)?;
    if (now_secs - ts_num).abs() > TOLERANCE_SECS {
        return Err(WebhookError::BadSignature);
    }
    standard_webhooks_verify(secret, id, ts, raw, sig)?;
    let v: Value = serde_json::from_slice(raw).map_err(|_| WebhookError::Malformed("not JSON".into()))?;
    let data = &v["data"];
    let kind_reason = match v["type"].as_str().unwrap_or("") {
        "payment.succeeded" if data["subscription_id"].is_null() || data["subscription_id"].as_str() == Some("") => (EntitlementKind::Granted, None),
        "subscription.active" | "subscription.renewed" => (EntitlementKind::Granted, None),
        "subscription.cancelled" => (EntitlementKind::Revoked, Some(RevokeReason::Cancelled)),
        "subscription.expired" => (EntitlementKind::Revoked, Some(RevokeReason::Expired)),
        "refund.succeeded" => (EntitlementKind::Revoked, Some(RevokeReason::Refund)),
        "dispute.opened" => (EntitlementKind::Revoked, Some(RevokeReason::Dispute)),
        _ => return Ok(None),
    };
    // Refunds and disputes carry the payment id; its metadata names the buyer.
    let meta_source = if matches!(kind_reason.1, Some(RevokeReason::Refund | RevokeReason::Dispute)) {
        let payment_id = data["payment_id"].as_str().ok_or_else(|| WebhookError::Malformed("missing payment_id".into()))?;
        if !payment_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(WebhookError::Malformed("bad payment_id".into()));
        }
        let url = format!("{}/payments/{payment_id}", api_base(acct.mode));
        let resp = transport.get(&url, &auth(acct)).await.map_err(|_| WebhookError::Malformed("payment lookup failed".into()))?;
        answer(resp.status, &resp.body).map_err(|_| WebhookError::Malformed("payment lookup failed".into()))?["metadata"].clone()
    } else {
        data["metadata"].clone()
    };
    let Some((meta_seller, buyer_ref, sku)) = meta_from(&meta_source) else {
        return Ok(None);
    };
    if meta_seller != seller_key {
        return Ok(None);
    }
    let expires_at_ms = data["next_billing_date"]
        .as_str()
        .and_then(parse_rfc3339_ms)
        .filter(|_| kind_reason.0 == EntitlementKind::Granted);
    Ok(Some(EntitlementEvent {
        kind: kind_reason.0,
        reason: kind_reason.1,
        seller_key: seller_key.to_owned(),
        buyer_ref,
        sku,
        provider_event_id: format!("dodo:{id}"),
        occurred_at_ms: ts_num * 1000,
        expires_at_ms,
    }))
}

/// `2026-10-29T12:00:00Z` (or with fractional seconds / an offset of Z) to epoch ms.
pub(crate) fn parse_rfc3339_ms(raw: &str) -> Option<i64> {
    let b = raw.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&raw[0..4])?, num(&raw[5..7])?, num(&raw[8..10])?);
    let (hh, mm, ss) = (num(&raw[11..13])?, num(&raw[14..16])?, num(&raw[17..19])?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400) + hh * 3_600 + mm * 60 + ss) * 1000)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use super::{create_checkout, create_product, parse_rfc3339_ms, product_body, verify_event};
    use crate::http::{HttpResponse, HttpTransport, TransportError};
    use crate::outbound::sign;
    use crate::seller::{CheckoutMeta, EntitlementKind, Interval, ProductSpec, RevokeReason, SellerAccount, SellerMode, SellerProvider};

    const WH: &str = "whsec_c2VsbGVyLXdlYmhvb2stc2VjcmV0LTMyLWJ5dGVzLTAwMA==";
    const NOW: i64 = 1_790_683_200;

    type Call = (String, String, Vec<(String, String)>, String);

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Call>>,
        answers: Mutex<Vec<(u16, String)>>,
    }

    #[async_trait]
    impl HttpTransport for Fake {
        async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push(("GET".into(), url.into(), headers.to_vec(), String::new()));
            let (status, body) = self.answers.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body: body.into_bytes() })
        }
        async fn post_json(&self, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push(("POST".into(), url.into(), headers.to_vec(), String::from_utf8_lossy(body).into()));
            let (status, body) = self.answers.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body: body.into_bytes() })
        }
    }

    fn acct() -> SellerAccount {
        SellerAccount { provider: SellerProvider::Dodo, mode: SellerMode::Test, secret: "seller_dodo_key".into(), webhook_secret: Some(WH.into()) }
    }

    fn signed(body: &serde_json::Value, id: &str) -> (HashMap<String, String>, Vec<u8>) {
        let raw = serde_json::to_vec(body).unwrap();
        let ts = NOW.to_string();
        let sig = sign(WH, id, &ts, &raw).unwrap();
        let h = HashMap::from([("webhook-id".to_owned(), id.to_owned()), ("webhook-timestamp".to_owned(), ts), ("webhook-signature".to_owned(), sig)]);
        (h, raw)
    }

    #[tokio::test]
    async fn products_and_checkouts_are_made_on_the_sellers_own_account() {
        let fake = Fake::default();
        fake.answers.lock().unwrap().extend([(200, json!({"product_id":"pdt_1"}).to_string()), (200, json!({"checkout_url":"https://test.checkout.dodopayments.com/s/1","session_id":"cks_1"}).to_string())]);
        let spec = ProductSpec { sku: "pro".into(), name: "Pro".into(), price_cents: 900, currency: "USD".into(), interval: Some(Interval::Month) };
        assert_eq!(create_product(&fake, &acct(), &spec).await.unwrap(), "pdt_1");
        let meta = CheckoutMeta { seller_key: "scribase:org.shop.production", buyer_ref: "user_1", sku: "pro" };
        let s = create_checkout(&fake, &acct(), "pdt_1", &meta, "https://shop.example.com/thanks").await.unwrap();
        assert_eq!(s.id, "cks_1");
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].1, "https://test.dodopayments.com/products");
        assert!(calls.iter().all(|c| c.2.contains(&("authorization".to_owned(), "Bearer seller_dodo_key".to_owned()))), "the seller's key, not ours");
        let body: serde_json::Value = serde_json::from_str(&calls[1].3).unwrap();
        assert_eq!(body["metadata"], json!({"mint_seller":"scribase:org.shop.production","buyer_ref":"user_1","sku":"pro"}));
        assert_eq!(product_body(&spec)["price"]["payment_frequency_interval"], "Month");
    }

    #[tokio::test]
    async fn webhooks_map_to_granted_and_revoked_for_this_seller_only() {
        let fake = Fake::default();
        let key = "scribase:org.shop.production";
        let meta = json!({"mint_seller": key, "buyer_ref": "user_1", "sku": "pro"});
        let (h, raw) = signed(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_1","subscription_id":null,"metadata":meta}}), "msg_1");
        let e = verify_event(&fake, &acct(), key, &h, &raw, NOW).await.unwrap().unwrap();
        assert_eq!((e.kind, e.buyer_ref.as_str(), e.sku.as_str(), e.provider_event_id.as_str()), (EntitlementKind::Granted, "user_1", "pro", "dodo:msg_1"));
        let (h, raw) = signed(&json!({"type":"subscription.active","data":{"subscription_id":"sub_1","next_billing_date":"2026-10-29T12:00:00Z","metadata":meta}}), "msg_2");
        let e = verify_event(&fake, &acct(), key, &h, &raw, NOW).await.unwrap().unwrap();
        assert_eq!(e.expires_at_ms, Some(1_793_275_200_000));
        fake.answers.lock().unwrap().push((200, json!({"payment_id":"pay_1","metadata":meta}).to_string()));
        let (h, raw) = signed(&json!({"type":"refund.succeeded","data":{"payment_id":"pay_1"}}), "msg_3");
        let e = verify_event(&fake, &acct(), key, &h, &raw, NOW).await.unwrap().unwrap();
        assert_eq!((e.kind, e.reason), (EntitlementKind::Revoked, Some(RevokeReason::Refund)));
        // Another seller's metadata changes nothing here.
        let (h, raw) = signed(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_2","metadata":{"mint_seller":"scribase:other","buyer_ref":"u","sku":"pro"}}}), "msg_4");
        assert!(verify_event(&fake, &acct(), key, &h, &raw, NOW).await.unwrap().is_none());
        // A subscription's own payment waits for subscription.active.
        let (h, raw) = signed(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_3","subscription_id":"sub_1","metadata":meta}}), "msg_5");
        assert!(verify_event(&fake, &acct(), key, &h, &raw, NOW).await.unwrap().is_none());
        // Bad signature and stale timestamp are refused.
        let (mut h, raw) = signed(&json!({"type":"payment.succeeded","data":{"metadata":meta}}), "msg_6");
        h.insert("webhook-signature".into(), "v1,AAAA".into());
        assert!(verify_event(&fake, &acct(), key, &h, &raw, NOW).await.is_err());
        let (h, raw) = signed(&json!({"type":"payment.succeeded","data":{"metadata":meta}}), "msg_7");
        assert!(verify_event(&fake, &acct(), key, &h, &raw, NOW + 3_600).await.is_err());
    }

    #[test]
    fn dates_parse_to_epoch_ms() {
        assert_eq!(parse_rfc3339_ms("2026-09-29T12:00:00Z"), Some(1_790_683_200_000));
        assert_eq!(parse_rfc3339_ms("2000-02-29T00:00:00.123Z"), Some(951_782_400_000));
        assert_eq!(parse_rfc3339_ms("nope"), None);
    }
}

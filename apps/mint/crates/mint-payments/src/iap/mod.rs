//! Mobile IAP: RevenueCat and Adapty (aggregators), and the App Store direct
//! ([`appstore`], Apple-signed JWS; no shared secret).
//!
//! These are webhook-ONLY on-ramps (the purchase happens SDK-side, so there is
//! no server checkout route). Webhook authenticity is an `Authorization`
//! shared-secret compared in CONSTANT TIME (via `subtle`), NOT an HMAC. Neither
//! provider exposes an arbitrary-payout API ⇒ `initiate_payout` returns
//! `ProviderUnavailable`.

pub mod adapty;
pub mod apple_jws;
pub mod appstore;
pub mod playstore;
pub mod revenuecat;

pub use adapty::AdaptyRamp;
pub use appstore::AppStoreRamp;
pub use playstore::PlayStoreRamp;
pub use revenuecat::RevenueCatRamp;

use serde_json::Value;

/// Find the purchase record whose store transaction id equals `txn` inside a
/// provider "customer" payload. `collections` are the containers to search
/// (e.g. RevenueCat `subscriber.non_subscriptions` / `subscriber.subscriptions`,
/// Adapty `data.non_subscriptions` / `data.subscriptions`). Each container may
/// be an array of records, an object of records keyed by product id, or an
/// object of ARRAYS of records keyed by product id — all three shapes occur in
/// the providers' customer APIs. A record matches when any of `id_keys` holds
/// exactly `txn`.
pub(crate) fn find_transaction<'a>(
    collections: &[&'a Value],
    id_keys: &[&str],
    txn: &str,
) -> Option<&'a Value> {
    fn matches(rec: &Value, id_keys: &[&str], txn: &str) -> bool {
        id_keys
            .iter()
            .any(|k| rec[*k].as_str().is_some_and(|v| !v.is_empty() && v == txn))
    }
    fn walk<'a>(v: &'a Value, id_keys: &[&str], txn: &str) -> Option<&'a Value> {
        match v {
            Value::Array(items) => items.iter().find_map(|item| walk(item, id_keys, txn)),
            Value::Object(map) => {
                if matches(v, id_keys, txn) {
                    return Some(v);
                }
                map.values()
                    .filter(|child| child.is_array() || child.is_object())
                    .find_map(|child| match child {
                        // Only descend one container level: a record's own nested
                        // objects (price, metadata) never hold other records.
                        Value::Array(items) => items.iter().find(|r| matches(r, id_keys, txn)),
                        Value::Object(_) if matches(child, id_keys, txn) => Some(child),
                        _ => None,
                    })
            }
            _ => None,
        }
    }
    collections.iter().find_map(|c| walk(c, id_keys, txn))
}

#[cfg(test)]
mod tests {
    use super::find_transaction;
    use serde_json::json;

    #[test]
    fn finds_records_in_every_container_shape() {
        let keyed_arrays = json!({"credits_10": [{"store_transaction_id": "t1"}]});
        let keyed_objects = json!({"pro_monthly": {"store_transaction_id": "t2"}});
        let flat = json!([{"store_transaction_id": "t3"}]);
        let keys = ["store_transaction_id"];
        let all = [&keyed_arrays, &keyed_objects, &flat];
        assert!(find_transaction(&all, &keys, "t1").is_some());
        assert!(find_transaction(&all, &keys, "t2").is_some());
        assert!(find_transaction(&all, &keys, "t3").is_some());
        assert!(find_transaction(&all, &keys, "nope").is_none());
        assert!(find_transaction(&all, &keys, "").is_none());
    }
}

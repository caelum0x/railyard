//! Live Postgres test for seller-own payments (Wave P PY2.3). `#[ignore]` +
//! gated on `DATABASE_URL` like pg_live.rs (a scratch database only):
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p mint-store-postgres --test pg_seller_live -- --ignored
//! ```

use std::sync::Arc;

use mint_receipts::FixedSigner;
use mint_store::{ConnectToken, EntitlementRecord, SellerDiscountRecord, SellerProductRecord, SellerRecord, SellerStore};
use mint_store_postgres::{PgSeller, PgStore};

fn seller(seller: &str, account: Option<&str>) -> SellerRecord {
    SellerRecord {
        product: "scribase".into(),
        seller: seller.into(),
        provider: if account.is_some() { "stripe".into() } else { "dodo".into() },
        mode: "test".into(),
        sealed_secret: "c2VhbGVk".into(),
        sealed_webhook_secret: None,
        account_ref: account.map(str::to_owned),
        connected_at_ms: 1,
    }
}

fn ent(id: &str, event: &str, kind: &str) -> EntitlementRecord {
    EntitlementRecord {
        id: id.into(),
        product: "scribase".into(),
        seller: "live.shop.production".into(),
        provider: "dodo".into(),
        provider_event_id: event.into(),
        kind: kind.into(),
        reason: (kind == "revoked").then(|| "refund".to_owned()),
        buyer_ref: "user_1".into(),
        sku: "pro".into(),
        occurred_at_ms: 10,
        expires_at_ms: None,
        recorded_at_ms: 1_000,
    }
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn sellers_products_tokens_and_the_entitlement_outbox() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PgStore::connect(&url, Arc::new(FixedSigner::new())).await.expect("connect");
    store.run_migrations().await.expect("migrations");
    for sql in [
        "DELETE FROM mint_seller_entitlements WHERE id LIKE 'ent_live_%'",
        "DELETE FROM mint_seller_discounts WHERE seller LIKE 'live.%'",
        "DELETE FROM mint_seller_products WHERE seller LIKE 'live.%'",
        "DELETE FROM mint_seller_accounts WHERE seller LIKE 'live.%'",
        "DELETE FROM mint_seller_connect_tokens WHERE seller LIKE 'live.%'",
    ] {
        sqlx::query(sql).execute(store.pool()).await.expect("clean");
    }
    let s = PgSeller::new(store.pool().clone());
    let hash = "a".repeat(64);
    s.put_token(ConnectToken { token_hash: hash.clone(), product: "scribase".into(), seller: "live.shop.production".into(), return_url: None, expires_at_ms: 5_000 }).await.expect("token");
    assert!(s.peek_token(&hash, 1_000).await.expect("peek").is_some());
    assert!(s.take_token(&hash, 1_000).await.expect("take").is_some());
    assert!(s.take_token(&hash, 1_000).await.expect("take again").is_none(), "one use");
    s.upsert_seller(seller("live.shop.production", None)).await.expect("seller");
    s.upsert_seller(seller("live.other.production", Some("acct_live123"))).await.expect("seller");
    assert_eq!(s.seller_by_account("stripe", "acct_live123").await.expect("by account").map(|r| r.seller), Some("live.other.production".into()));
    let p = SellerProductRecord { product: "scribase".into(), seller: "live.shop.production".into(), sku: "pro".into(), name: "Pro".into(), price_cents: 900, currency: "USD".into(), interval: Some("month".into()), provider_ref: "pdt_1".into(), created_at_ms: 1 };
    assert_eq!(s.put_product(p.clone()).await.expect("product").provider_ref, "pdt_1");
    assert_eq!(s.put_product(SellerProductRecord { provider_ref: "pdt_2".into(), ..p }).await.expect("again").provider_ref, "pdt_1", "the first stays");
    let d = SellerDiscountRecord { product: "scribase".into(), seller: "live.shop.production".into(), discount_id: "dsc_live_1".into(), code: "LAUNCH20".into(), created_at_ms: 1 };
    s.put_discount(d.clone()).await.expect("discount");
    s.put_discount(SellerDiscountRecord { code: "LAUNCH25".into(), created_at_ms: 2, ..d.clone() }).await.expect("re-code");
    assert_eq!(s.get_discount("scribase", "live.shop.production", "dsc_live_1").await.expect("get").map(|r| (r.code, r.created_at_ms)), Some(("LAUNCH25".into(), 1)));
    assert!(s.get_discount("scribase", "live.other.production", "dsc_live_1").await.expect("other").is_none(), "another seller never sees it");
    assert_eq!(s.discount_by_code("scribase", "live.shop.production", "LAUNCH25").await.expect("by code").map(|r| r.discount_id), Some("dsc_live_1".into()));
    assert!(s.record_entitlement(ent("ent_live_1", "dodo:msg_live_1", "granted")).await.expect("record"));
    assert!(!s.record_entitlement(ent("ent_live_2", "dodo:msg_live_1", "granted")).await.expect("replay"));
    assert!(s.due(1_000, 100).await.expect("due").iter().any(|d| d.record.id == "ent_live_1"));
    s.failed("ent_live_1", 9_000, "HTTP 500").await.expect("failed");
    assert!(!s.due(1_000, 100).await.expect("due").iter().any(|d| d.record.id == "ent_live_1"));
    s.delivered("ent_live_1", 9_500).await.expect("delivered");
    assert!(!s.due(i64::MAX, 100).await.expect("due").iter().any(|d| d.record.id == "ent_live_1"));
    assert_eq!(s.list_entitlements("scribase", "live.shop.production", 10).await.expect("list").len(), 1);
}

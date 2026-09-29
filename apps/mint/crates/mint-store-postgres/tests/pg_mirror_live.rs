//! Live Postgres test for the portfolio mirror (Wave P PY1.5). `#[ignore]` +
//! gated on `DATABASE_URL` like pg_live.rs:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p mint-store-postgres --test pg_mirror_live -- --ignored
//! ```

use std::sync::Arc;

use mint_receipts::FixedSigner;
use mint_store::{MirrorEvent, MirrorKind, MirrorStore};
use mint_store_postgres::{PgMirror, PgStore};

fn event(id: &str, provider_event_id: &str) -> MirrorEvent {
    MirrorEvent {
        id: id.into(),
        product: "scribase".into(),
        provider: "dodo".into(),
        provider_event_id: provider_event_id.into(),
        kind: MirrorKind::Payment,
        amount_micros: 10_000_000,
        currency: "USD".into(),
        customer_ref: "org_acme".into(),
        occurred_at_ms: 1,
        recorded_at_ms: 1_000,
    }
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn mirror_records_once_and_delivers_through_the_outbox() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PgStore::connect(&url, Arc::new(FixedSigner::new())).await.expect("connect");
    store.run_migrations().await.expect("migrations");
    sqlx::query("DELETE FROM mint_mirror_deliveries WHERE event_id LIKE 'mev_live_%'").execute(store.pool()).await.expect("clean");
    sqlx::query("DELETE FROM mint_mirror_events WHERE id LIKE 'mev_live_%'").execute(store.pool()).await.expect("clean");
    let mirror = PgMirror::new(store.pool().clone());
    let endpoints = vec!["scribase-billing".to_string()];
    assert!(mirror.record(event("mev_live_1", "pay_live_1"), &endpoints).await.expect("record"));
    assert!(!mirror.record(event("mev_live_2", "pay_live_1"), &endpoints).await.expect("replay"), "one row per provider event");
    assert_eq!(mirror.list("scribase", 10).await.expect("list").iter().filter(|e| e.id.starts_with("mev_live_")).count(), 1);
    assert!(mirror.list("rally", 10).await.expect("list").iter().all(|e| e.product == "rally"));
    let due = mirror.due(1_000, 100).await.expect("due");
    assert!(due.iter().any(|d| d.event.id == "mev_live_1" && d.endpoint == "scribase-billing"));
    mirror.failed("mev_live_1", "scribase-billing", 5_000, "HTTP 500").await.expect("failed");
    assert!(!mirror.due(1_000, 100).await.expect("due").iter().any(|d| d.event.id == "mev_live_1"));
    mirror.delivered("mev_live_1", "scribase-billing", 6_000).await.expect("delivered");
    assert!(!mirror.due(i64::MAX, 100).await.expect("due").iter().any(|d| d.event.id == "mev_live_1"));
}

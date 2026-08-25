//! Purchase-flow tests against a contract-faithful FakeMint.
//!
//! Proves the game store settles on Mint exactly like every other Railyard
//! consumer: hold -> capture on success (grant), hold -> release on failure
//! (grant nothing), idempotent retries never double-charge, and each SKU shape
//! grants the right entitlement.

use async_trait::async_trait;

use game_economy_core::{
    content_item_id, entitlement_id, Catalog, Fulfiller, GameError, GameStore, Product, Rarity,
};
use game_economy_testkit::{Call, FakeMint};

const PLAYER: &str = "acct_player";
const REVENUE: &str = "acct_revenue";

fn is_capture(c: &Call) -> bool {
    matches!(c, Call::Capture { .. })
}
fn is_hold(c: &Call) -> bool {
    matches!(c, Call::CreateHold { .. })
}
fn is_release(c: &Call) -> bool {
    matches!(c, Call::Release { .. })
}

/// (a) A pack purchase does hold -> capture exactly once and grants the pack.
#[tokio::test]
async fn pack_purchase_holds_captures_once_and_grants() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let out = store
        .purchase(PLAYER, "pack.standard", "buy-1")
        .await
        .expect("purchase should succeed");

    // Charged the exact catalog price, once, to revenue.
    assert_eq!(out.charged_micros, 990_000);
    assert!(!out.idempotent_replay);
    assert_eq!(store.count_calls_hold(), 1);
    assert_eq!(store.count_calls_capture(), 1);
    assert_eq!(store.count_calls_release(), 0);

    // Money moved: player debited, revenue credited.
    assert_eq!(store.mint_balance(PLAYER), 10_000_000 - 990_000);
    assert_eq!(store.mint_balance(REVENUE), 990_000);

    // Pack granted, sealed (not yet opened).
    let inv = store.inventory(PLAYER);
    assert_eq!(inv.packs.len(), 1);
    let pack = inv.packs.get(&out.entitlement_id).unwrap();
    assert_eq!(pack.pack_sku, "pack.standard");
    assert!(!pack.is_opened());
    assert_eq!(pack.receipt_id, out.receipt_id);
}

/// A fulfiller that always refuses — exercises the release path.
struct AlwaysFail;
#[async_trait]
impl Fulfiller for AlwaysFail {
    async fn fulfill(&self, _player: &str, _product: &Product) -> Result<(), GameError> {
        Err(GameError::FulfillmentFailed("out of stock".into()))
    }
}

/// (b) A failed purchase releases the hold and grants nothing (no charge).
#[tokio::test]
async fn failed_purchase_releases_and_grants_nothing() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::with_fulfiller(mint, AlwaysFail, Catalog::demo(), REVENUE);

    let err = store
        .purchase(PLAYER, "pack.premium", "buy-2")
        .await
        .expect_err("fulfillment failure should error");
    assert_eq!(err, GameError::FulfillmentFailed("out of stock".into()));

    // Hold opened then released; never captured.
    assert_eq!(store.count_calls_hold(), 1);
    assert_eq!(store.count_calls_release(), 1);
    assert_eq!(store.count_calls_capture(), 0);

    // No money moved (reservation fully returned) and nothing granted.
    assert_eq!(store.mint_balance(PLAYER), 10_000_000);
    assert_eq!(store.mint_balance(REVENUE), 0);
    assert!(store.inventory(PLAYER).packs.is_empty());
}

/// (c) An idempotent retry doesn't double-charge and doesn't duplicate the grant.
#[tokio::test]
async fn idempotent_retry_does_not_double_charge() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let first = store.purchase(PLAYER, "pack.mega", "buy-3").await.unwrap();
    let second = store.purchase(PLAYER, "pack.mega", "buy-3").await.unwrap();

    assert!(!first.idempotent_replay);
    assert!(second.idempotent_replay, "replay must be flagged");
    assert_eq!(first.entitlement_id, second.entitlement_id);
    assert_eq!(first.receipt_id, second.receipt_id);

    // Debited exactly once despite two calls.
    assert_eq!(store.mint_balance(PLAYER), 10_000_000 - 9_990_000);
    assert_eq!(store.mint_balance(REVENUE), 9_990_000);
    // Exactly one pack granted.
    assert_eq!(store.inventory(PLAYER).packs.len(), 1);
}

/// Insufficient funds fails at the hold — no capture, no grant.
#[tokio::test]
async fn underfunded_purchase_never_charges_or_grants() {
    let mint = FakeMint::new().with_balance(PLAYER, 100_000); // < $0.99
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let err = store
        .purchase(PLAYER, "pack.standard", "buy-4")
        .await
        .unwrap_err();
    assert_eq!(err, GameError::InsufficientFunds);
    assert_eq!(store.count_calls_capture(), 0);
    assert!(store.inventory(PLAYER).packs.is_empty());
    assert_eq!(store.mint_balance(PLAYER), 100_000);
}

/// (e) A cosmetic purchase grants the (content-addressed) cosmetic entitlement.
#[tokio::test]
async fn cosmetic_purchase_grants_cosmetic() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let out = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin")
        .await
        .unwrap();
    assert_eq!(out.charged_micros, 4_990_000);

    let inv = store.inventory(PLAYER);
    // The bought skin shares the canonical content id with its pack-drop twin.
    let expected = content_item_id(Rarity::Legendary, "Aeon Rift Blade");
    assert!(inv.owns_cosmetic(&expected));
    assert_eq!(inv.cosmetics.get(&expected).map(|s| s.as_str()), Some("Aeon Rift Blade"));

    // Re-buying the same skin is rejected pre-hold (never double-charged).
    let err = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin-again")
        .await
        .unwrap_err();
    assert_eq!(err, GameError::AlreadyOwned("skin.aeon.blade".into()));
    assert_eq!(store.mint_balance(REVENUE), 4_990_000); // still charged only once
}

/// A battle-pass purchase grants the pass at tier 1 and settles on Mint.
#[tokio::test]
async fn battle_pass_purchase_grants_pass() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let out = store
        .purchase(PLAYER, "pass.season1", "buy-pass")
        .await
        .unwrap();
    assert_eq!(out.charged_micros, 9_990_000);

    let inv = store.inventory(PLAYER);
    assert!(inv.owns_battle_pass("pass.season1"));
    let bp = inv.battle_pass.expect("pass granted");
    assert_eq!(bp.pass_sku, "pass.season1");
    assert_eq!(bp.tier, 1);
}

/// An unknown SKU fails before any Mint call.
#[tokio::test]
async fn unknown_product_never_touches_mint() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let err = store.purchase(PLAYER, "nope.sku", "x").await.unwrap_err();
    assert_eq!(err, GameError::UnknownProduct("nope.sku".into()));
    assert_eq!(store.total_calls(), 0);
}

/// The deterministic entitlement id is a pure function of (player, idem_key).
#[test]
fn entitlement_ids_are_deterministic() {
    assert_eq!(entitlement_id(PLAYER, "buy-1"), entitlement_id(PLAYER, "buy-1"));
    assert_ne!(entitlement_id(PLAYER, "buy-1"), entitlement_id(PLAYER, "buy-2"));
}

// --- small test-only accessors kept out of the public store API ---------------

trait StoreProbe {
    fn count_calls_hold(&self) -> usize;
    fn count_calls_capture(&self) -> usize;
    fn count_calls_release(&self) -> usize;
    fn total_calls(&self) -> usize;
    fn mint_balance(&self, account: &str) -> i64;
}

impl<F: Fulfiller> StoreProbe for GameStore<FakeMint, F> {
    fn count_calls_hold(&self) -> usize {
        self.mint().count_calls(is_hold)
    }
    fn count_calls_capture(&self) -> usize {
        self.mint().count_calls(is_capture)
    }
    fn count_calls_release(&self) -> usize {
        self.mint().count_calls(is_release)
    }
    fn total_calls(&self) -> usize {
        self.mint().calls().len()
    }
    fn mint_balance(&self, account: &str) -> i64 {
        self.mint().balance(account)
    }
}

//! Purchase-flow tests against a contract-faithful FakeMint.
//!
//! Proves the game store settles on Mint exactly like every other Railyard
//! consumer: hold -> capture on success (grant), hold -> release on failure
//! (grant nothing), idempotent retries never double-charge, and each SKU shape
//! grants the right entitlement.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use game_economy_core::{
    content_item_id, entitlement_id, Catalog, CompletedPurchase, Fulfiller, GameError,
    GameStateRepository, GameStore, Inventory, MemoryGameState, Product, PurchaseRecord, Rarity,
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

/// Simulates a process/storage crash in the only dangerous window: Mint has
/// captured successfully, but the local entitlement transaction has not yet
/// committed. The persisted pending intent lets a retry replay Mint and recover
/// ownership without charging twice.
struct FailFirstCommit {
    inner: MemoryGameState,
    fail: AtomicBool,
}

impl FailFirstCommit {
    fn new() -> Self {
        Self {
            inner: MemoryGameState::default(),
            fail: AtomicBool::new(true),
        }
    }
}

impl GameStateRepository for FailFirstCommit {
    fn inventory(&self, player: &str) -> Result<Inventory, GameError> {
        self.inner.inventory(player)
    }
    fn purchase(&self, key: &str) -> Result<Option<PurchaseRecord>, GameError> {
        self.inner.purchase(key)
    }
    fn purchases_for(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError> {
        self.inner.purchases_for(player)
    }
    fn begin_purchase(&self, key: &str, player: &str, sku: &str) -> Result<(), GameError> {
        self.inner.begin_purchase(key, player, sku)
    }
    fn commit_purchase(
        &self,
        key: &str,
        completed: &CompletedPurchase,
        inventory: &Inventory,
    ) -> Result<(), GameError> {
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(GameError::Storage("simulated crash after capture".into()));
        }
        self.inner.commit_purchase(key, completed, inventory)
    }
    fn save_inventory(&self, player: &str, inventory: &Inventory) -> Result<(), GameError> {
        self.inner.save_inventory(player, inventory)
    }
    fn reverse_purchase(
        &self,
        player: &str,
        receipt_id: &str,
        reversal: &game_economy_core::PurchaseReversal,
        ownership_key: Option<&str>,
        revoke: &dyn Fn(&CompletedPurchase, &mut Inventory),
    ) -> Result<game_economy_core::ReverseOutcome, GameError> {
        self.inner
            .reverse_purchase(player, receipt_id, reversal, ownership_key, revoke)
    }
    fn recent_purchases(&self, limit: usize) -> Result<Vec<CompletedPurchase>, GameError> {
        self.inner.recent_purchases(limit)
    }
    fn summary(&self) -> Result<game_economy_core::PurchaseSummary, GameError> {
        self.inner.summary()
    }
    fn reserve_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        sku: &str,
        idem_key: &str,
    ) -> Result<(), GameError> {
        self.inner
            .reserve_single_owner(player, ownership_key, sku, idem_key)
    }
    fn release_single_owner(&self, player: &str, ownership_key: &str, idem_key: &str) {
        self.inner
            .release_single_owner(player, ownership_key, idem_key)
    }

    fn single_owner_holder(
        &self,
        player: &str,
        ownership_key: &str,
    ) -> Result<Option<game_economy_core::ReservationHolder>, GameError> {
        self.inner.single_owner_holder(player, ownership_key)
    }

    fn take_over_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        from_idem: &str,
        to_idem: &str,
    ) -> Result<bool, GameError> {
        self.inner.take_over_single_owner(player, ownership_key, from_idem, to_idem)
    }
}

#[tokio::test]
async fn retry_recovers_entitlement_after_capture_commit_crash() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let state = Arc::new(FailFirstCommit::new());
    let store = GameStore::with_state(
        mint,
        game_economy_core::AlwaysFulfill,
        Catalog::demo(),
        REVENUE,
        state,
    );

    let first = store.purchase(PLAYER, "pack.standard", "recover-1").await;
    assert_eq!(
        first.unwrap_err(),
        GameError::Storage("simulated crash after capture".into())
    );
    assert!(store.inventory(PLAYER).packs.is_empty());
    assert_eq!(store.mint_balance(REVENUE), 990_000);

    let recovered = store
        .purchase(PLAYER, "pack.standard", "recover-1")
        .await
        .expect("retry should recover the captured entitlement");
    assert!(recovered.idempotent_replay);
    assert_eq!(store.mint_balance(REVENUE), 990_000);
    assert_eq!(store.inventory(PLAYER).packs.len(), 1);
    assert_eq!(
        recovered.receipt_id,
        recovered.settlement_proof.receipt_hash
    );
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
    assert_eq!(
        inv.cosmetics.get(&expected).map(|s| s.as_str()),
        Some("Aeon Rift Blade")
    );

    // Re-buying the same skin is rejected pre-hold (never double-charged).
    let err = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin-again")
        .await
        .unwrap_err();
    assert_eq!(err, GameError::AlreadyOwned("skin.aeon.blade".into()));
    assert_eq!(store.mint_balance(REVENUE), 4_990_000); // still charged only once
}

/// (e2) An idempotent RETRY of a cosmetic (same idem_key) replays the original
/// outcome instead of being rejected as `AlreadyOwned`, and never double-charges.
#[tokio::test]
async fn idempotent_retry_of_cosmetic_replays_not_rejected() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let first = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin")
        .await
        .expect("first cosmetic purchase succeeds");
    assert!(!first.idempotent_replay);

    // Same idem_key retry: MUST replay, not reject.
    let second = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin")
        .await
        .expect("idempotent retry must replay, not error AlreadyOwned");
    assert!(
        second.idempotent_replay,
        "retry must be flagged as a replay"
    );
    assert_eq!(first.entitlement_id, second.entitlement_id);
    assert_eq!(first.receipt_id, second.receipt_id);
    assert_eq!(first.charged_micros, second.charged_micros);

    // Charged exactly once despite the retry.
    assert_eq!(store.mint_balance(REVENUE), 4_990_000);
    assert_eq!(store.mint_balance(PLAYER), 10_000_000 - 4_990_000);
    // Exactly one cosmetic owned.
    assert_eq!(store.inventory(PLAYER).cosmetics.len(), 1);
}

/// (e3) An idempotent RETRY of a battle-pass (same idem_key) replays too — the
/// single-owner guard must not shadow a legitimate retry.
#[tokio::test]
async fn idempotent_retry_of_battle_pass_replays_not_rejected() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let first = store
        .purchase(PLAYER, "pass.season1", "buy-pass")
        .await
        .unwrap();
    let second = store
        .purchase(PLAYER, "pass.season1", "buy-pass")
        .await
        .expect("idempotent retry must replay, not error AlreadyOwned");
    assert!(!first.idempotent_replay);
    assert!(second.idempotent_replay);
    assert_eq!(first.entitlement_id, second.entitlement_id);
    assert_eq!(store.mint_balance(REVENUE), 9_990_000); // charged once
    assert!(store.inventory(PLAYER).owns_battle_pass("pass.season1"));
}

/// (e4) Reusing one idem_key for a DIFFERENT SKU is an idempotency conflict, not a
/// silent replay of the wrong product.
#[tokio::test]
async fn same_idem_key_different_sku_is_conflict() {
    let mint = FakeMint::new().with_balance(PLAYER, 20_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    store
        .purchase(PLAYER, "pack.standard", "dup-key")
        .await
        .unwrap();
    let err = store
        .purchase(PLAYER, "skin.aeon.blade", "dup-key")
        .await
        .expect_err("same key, different SKU must conflict");
    assert_eq!(err, GameError::IdempotencyConflict);
    // The conflicting call never charged for the skin.
    assert_eq!(store.mint_balance(REVENUE), 990_000);
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

/// A purchased battle-pass advances tiers (monotonic, clamped) and moves NO money.
#[tokio::test]
async fn battle_pass_tiers_advance_after_purchase_without_charging() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    store
        .purchase(PLAYER, "pass.season1", "buy-pass")
        .await
        .unwrap();
    let revenue_after_buy = store.mint_balance(REVENUE);

    // Advance to tier 40, then a stale lower report is a no-op, then saturate.
    assert_eq!(
        store
            .advance_battle_pass(PLAYER, "pass.season1", 40)
            .unwrap(),
        40
    );
    assert_eq!(
        store
            .advance_battle_pass(PLAYER, "pass.season1", 5)
            .unwrap(),
        40,
        "a lower report must never regress the pass"
    );
    assert_eq!(
        store
            .advance_battle_pass(PLAYER, "pass.season1", 9_999)
            .unwrap(),
        100,
        "advance saturates at the season's tier count"
    );

    // Progression is persisted on the inventory.
    assert_eq!(store.inventory(PLAYER).battle_pass.unwrap().tier, 100);
    // No money moved during progression — the pass was paid for exactly once.
    assert_eq!(store.mint_balance(REVENUE), revenue_after_buy);
}

/// Advancing a pass the player does not own is rejected (no silent create).
#[tokio::test]
async fn advancing_an_unowned_pass_errors() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let err = store
        .advance_battle_pass(PLAYER, "pass.season1", 5)
        .unwrap_err();
    assert_eq!(err, GameError::BattlePassNotOwned("pass.season1".into()));
    assert!(store.inventory(PLAYER).battle_pass.is_none());
}

/// Advancing a non-pass SKU (or unknown SKU) is an unknown-product error.
#[tokio::test]
async fn advancing_a_non_pass_sku_errors() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    assert_eq!(
        store
            .advance_battle_pass(PLAYER, "pack.standard", 5)
            .unwrap_err(),
        GameError::UnknownProduct("pack.standard".into())
    );
    assert_eq!(
        store
            .advance_battle_pass(PLAYER, "no.such.sku", 5)
            .unwrap_err(),
        GameError::UnknownProduct("no.such.sku".into())
    );
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
    assert_eq!(
        entitlement_id(PLAYER, "buy-1"),
        entitlement_id(PLAYER, "buy-1")
    );
    assert_ne!(
        entitlement_id(PLAYER, "buy-1"),
        entitlement_id(PLAYER, "buy-2")
    );
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

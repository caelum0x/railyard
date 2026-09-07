//! Money invariants for the game storefront, verified over the whole
//! hold → capture / release lifecycle.
//!
//! The per-scenario tests in `purchase_flow.rs` assert specific balances; this
//! file instead pins the two properties that must hold *no matter what sequence of
//! purchases runs*:
//!
//!   1. **No negative balance / no negative hold** — no account is ever overdrawn
//!      and no reservation is ever negative.
//!   2. **Conservation of money** — `Σ account balances + Σ open holds` is constant
//!      and equals the initially seeded supply. Money only ever *moves* between the
//!      available and held buckets, or between the player and the revenue account;
//!      none is created or destroyed.
//!
//! These are checked after every step of the lifecycle, across the happy path,
//! the underfunded path, the failed-fulfillment release, idempotent replays, a
//! post-capture-commit crash + recovery, and finally a deterministic fuzz loop of
//! pseudo-random purchase sequences (a property test with no extra dependency).

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use game_economy_core::transport::{Capture, Hold, MintTransport, Release};
use game_economy_core::{
    AlwaysFulfill, Catalog, CompletedPurchase, Fulfiller, GameError, GameStateRepository,
    GameStore, Inventory, MemoryGameState, Product, PurchaseRecord,
};
use game_economy_testkit::FakeMint;

const PLAYER: &str = "acct_player";
const REVENUE: &str = "acct_revenue";

/// The two invariants, checked against a known seeded supply.
fn assert_invariants(mint: &FakeMint, seeded_supply: i64) {
    for (account, balance) in mint.accounts() {
        assert!(
            balance >= 0,
            "account {account} went negative: {balance} µ$"
        );
    }
    for amount in mint.open_holds() {
        assert!(amount >= 0, "open hold went negative: {amount} µ$");
    }
    assert_eq!(
        mint.conserved_total(),
        seeded_supply,
        "money was created or destroyed: conserved={} seeded={}",
        mint.conserved_total(),
        seeded_supply
    );
}

/// The direct hold → capture path conserves money at every step: reserving moves
/// available → held, capturing settles exactly `price` to revenue with zero refund
/// on a full-price good, and closes the hold (held → 0).
#[tokio::test]
async fn hold_then_capture_conserves_and_never_goes_negative() {
    let supply = 10_000_000;
    let price = 990_000;
    let mint = FakeMint::new().with_balance(PLAYER, supply);
    assert_invariants(&mint, supply);

    let hold = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: price,
            idem_key: "k:hold".into(),
        })
        .await
        .unwrap();
    // Available dropped by price; the same amount is now held.
    assert_eq!(mint.balance(PLAYER), supply - price);
    assert_eq!(mint.held_total(), price);
    assert_invariants(&mint, supply);

    let cap = mint
        .capture_hold(
            &hold.hold_id,
            Capture {
                amount_micros: price,
                to: REVENUE.into(),
                idem_key: "k:capture".into(),
            },
        )
        .await
        .unwrap();
    // Player debited exactly price == revenue credited; no refund; hold closed.
    assert_eq!(cap.captured_micros, price);
    assert_eq!(mint.balance(PLAYER), supply - price);
    assert_eq!(mint.balance(REVENUE), price);
    assert_eq!(mint.held_total(), 0);
    assert_invariants(&mint, supply);
}

/// The direct hold → release path returns the whole reservation and settles
/// nothing, conserving money throughout.
#[tokio::test]
async fn hold_then_release_returns_everything() {
    let supply = 5_000_000;
    let price = 2_990_000;
    let mint = FakeMint::new().with_balance(PLAYER, supply);

    let hold = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: price,
            idem_key: "k:hold".into(),
        })
        .await
        .unwrap();
    assert_eq!(mint.held_total(), price);
    assert_invariants(&mint, supply);

    mint.release_hold(
        &hold.hold_id,
        Release {
            idem_key: "k:release".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(mint.balance(PLAYER), supply); // fully restored
    assert_eq!(mint.balance(REVENUE), 0);
    assert_eq!(mint.held_total(), 0);
    assert_invariants(&mint, supply);
}

/// A successful purchase moves exactly `price` from player to revenue and leaves
/// nothing held; invariants hold before and after.
#[tokio::test]
async fn successful_purchase_conserves_money() {
    let supply = 10_000_000;
    let store = GameStore::new(
        FakeMint::new().with_balance(PLAYER, supply),
        Catalog::demo(),
        REVENUE,
    );
    assert_invariants(store.mint(), supply);

    let out = store
        .purchase(PLAYER, "pack.standard", "buy-1")
        .await
        .unwrap();
    let debited = supply - store.mint().balance(PLAYER);
    let credited = store.mint().balance(REVENUE);
    assert_eq!(debited, out.charged_micros);
    assert_eq!(credited, out.charged_micros);
    assert_eq!(store.mint().held_total(), 0);
    assert_invariants(store.mint(), supply);
}

/// An underfunded purchase fails at the hold: no money moves, nothing held.
#[tokio::test]
async fn underfunded_purchase_conserves_money() {
    let supply = 100_000; // < $0.99
    let store = GameStore::new(
        FakeMint::new().with_balance(PLAYER, supply),
        Catalog::demo(),
        REVENUE,
    );

    let err = store
        .purchase(PLAYER, "pack.standard", "poor-1")
        .await
        .unwrap_err();
    assert_eq!(err, GameError::InsufficientFunds);
    assert_eq!(store.mint().balance(PLAYER), supply);
    assert_eq!(store.mint().balance(REVENUE), 0);
    assert_invariants(store.mint(), supply);
}

/// A fulfiller that always refuses — drives the release path.
struct AlwaysFail;
#[async_trait]
impl Fulfiller for AlwaysFail {
    async fn fulfill(&self, _player: &str, _product: &Product) -> Result<(), GameError> {
        Err(GameError::FulfillmentFailed("out of stock".into()))
    }
}

/// A failed fulfillment releases the hold: the reservation is fully returned and
/// nothing settles, so money is conserved.
#[tokio::test]
async fn failed_fulfillment_release_conserves_money() {
    let supply = 10_000_000;
    let store = GameStore::with_fulfiller(
        FakeMint::new().with_balance(PLAYER, supply),
        AlwaysFail,
        Catalog::demo(),
        REVENUE,
    );

    let err = store
        .purchase(PLAYER, "pack.premium", "fail-1")
        .await
        .unwrap_err();
    assert_eq!(err, GameError::FulfillmentFailed("out of stock".into()));
    assert_eq!(store.mint().balance(PLAYER), supply);
    assert_eq!(store.mint().balance(REVENUE), 0);
    assert_eq!(store.mint().held_total(), 0);
    assert_invariants(store.mint(), supply);
}

/// An idempotent replay moves ZERO additional money: the second call returns the
/// original outcome and the ledger is byte-for-byte where the first call left it.
#[tokio::test]
async fn idempotent_replay_moves_no_additional_money() {
    let supply = 10_000_000;
    let store = GameStore::new(
        FakeMint::new().with_balance(PLAYER, supply),
        Catalog::demo(),
        REVENUE,
    );

    store.purchase(PLAYER, "pack.mega", "buy-1").await.unwrap();
    let accounts_after_first = store.mint().accounts();
    let captures_after_first = store
        .mint()
        .count_calls(|c| matches!(c, game_economy_testkit::Call::Capture { .. }));

    let replay = store.purchase(PLAYER, "pack.mega", "buy-1").await.unwrap();
    assert!(replay.idempotent_replay);
    // Balances are identical — the replay debited/credited nothing.
    assert_eq!(store.mint().accounts(), accounts_after_first);
    // And it opened no new capture on the rail (Mint collapsed the retry).
    assert_eq!(
        store
            .mint()
            .count_calls(|c| matches!(c, game_economy_testkit::Call::Capture { .. })),
        captures_after_first
    );
    assert_invariants(store.mint(), supply);
}

/// Simulates a crash in the only dangerous window: Mint captured, but the local
/// entitlement commit had not landed. Recovery replays the deterministic Mint keys
/// and must not move a second dollar. Money is conserved across the crash.
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
}

#[tokio::test]
async fn crash_after_capture_then_recovery_conserves_money() {
    let supply = 10_000_000;
    let state = Arc::new(FailFirstCommit::new());
    let store = GameStore::with_state(
        FakeMint::new().with_balance(PLAYER, supply),
        AlwaysFulfill,
        Catalog::demo(),
        REVENUE,
        state,
    );

    // First attempt: Mint captured, local commit "crashed".
    let err = store
        .purchase(PLAYER, "pack.standard", "recover-1")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        GameError::Storage("simulated crash after capture".into())
    );
    let revenue_after_crash = store.mint().balance(REVENUE);
    assert!(revenue_after_crash > 0, "capture happened before the crash");
    assert_eq!(store.mint().held_total(), 0);
    assert_invariants(store.mint(), supply);

    // Retry recovers ownership by replaying the same keys — not a second charge.
    let recovered = store
        .purchase(PLAYER, "pack.standard", "recover-1")
        .await
        .unwrap();
    assert!(recovered.idempotent_replay);
    assert_eq!(store.mint().balance(REVENUE), revenue_after_crash);
    assert_invariants(store.mint(), supply);
}

// --- deterministic fuzz: property coverage without a proptest dependency -------

/// A fulfiller whose success/failure is flipped by a shared flag before each
/// purchase, so the fuzz loop can drive both the capture and the release path.
#[derive(Clone)]
struct ToggleFulfiller {
    fail: Arc<AtomicBool>,
}
#[async_trait]
impl Fulfiller for ToggleFulfiller {
    async fn fulfill(&self, _player: &str, _product: &Product) -> Result<(), GameError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(GameError::FulfillmentFailed("fuzz-refused".into()))
        } else {
            Ok(())
        }
    }
}

/// A reproducible pseudo-random `u64` drawn from blake3 — same seed+step+label
/// always yields the same value, so a failing fuzz case is fully reproducible.
fn draw(seed: u64, step: u64, label: &str) -> u64 {
    let pre = format!("fuzz:v1:{seed}:{step}:{label}");
    let h = blake3::hash(pre.as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    u64::from_le_bytes(b)
}

/// Fire many pseudo-random purchase sequences over a fixed funded supply shared by
/// several players, re-checking BOTH invariants after every single operation. This
/// is property-style coverage over an adversarial input space (idempotent replays,
/// idempotency conflicts, already-owned single-owner goods, underfunded holds, and
/// failed fulfillments all occur), with no extra test dependency.
#[tokio::test]
async fn fuzzed_purchase_sequences_preserve_invariants() {
    // A fixed set of players, each seeded — the conserved supply is their sum.
    const PLAYERS: &[&str] = &["acct_a", "acct_b", "acct_c"];
    const PER_PLAYER: i64 = 50_000_000;
    // A mix of consumable, single-owner, and non-existent SKUs.
    const SKUS: &[&str] = &[
        "pack.standard",
        "pack.premium",
        "pack.mega",
        "skin.aeon.blade",
        "skin.rift.cloak",
        "pass.season1",
        "no.such.sku",
    ];
    let seeded_supply = PER_PLAYER * PLAYERS.len() as i64;

    for seed in 0u64..64 {
        // Fresh ledger + store per seed.
        let mut mint = FakeMint::new();
        for p in PLAYERS {
            mint = mint.with_balance(p, PER_PLAYER);
        }
        let flag = Arc::new(AtomicBool::new(false));
        let fulfiller = ToggleFulfiller { fail: flag.clone() };
        let store = GameStore::with_fulfiller(mint, fulfiller, Catalog::demo(), REVENUE);
        assert_invariants(store.mint(), seeded_supply);

        let mut used_keys: Vec<String> = Vec::new();

        for step in 0u64..24 {
            let player = PLAYERS[(draw(seed, step, "player") % PLAYERS.len() as u64) as usize];
            let sku = SKUS[(draw(seed, step, "sku") % SKUS.len() as u64) as usize];

            // ~30% of the time reuse a prior key (exercises replay + conflict),
            // otherwise mint a fresh deterministic key.
            let reuse = !used_keys.is_empty() && draw(seed, step, "reuse") % 10 < 3;
            let idem_key = if reuse {
                used_keys[(draw(seed, step, "which") % used_keys.len() as u64) as usize].clone()
            } else {
                let k = format!("s{seed}-t{step}");
                used_keys.push(k.clone());
                k
            };

            // Flip fulfillment success for this op.
            flag.store(draw(seed, step, "fulfill").is_multiple_of(4), Ordering::SeqCst);

            // The result may be Ok or any Err — what matters is the ledger stays
            // consistent regardless of outcome.
            let _ = store.purchase(player, sku, &idem_key).await;
            assert_invariants(store.mint(), seeded_supply);
        }
    }
}

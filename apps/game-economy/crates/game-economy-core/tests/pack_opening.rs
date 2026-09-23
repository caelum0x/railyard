//! Pack-opening determinism (d) + FakeMint contract-fidelity tests.

use game_economy_core::transport::{Capture, Hold, Release};
use game_economy_core::{Catalog, GameError, GameStore, MintTransport};
use game_economy_testkit::FakeMint;

const PLAYER: &str = "acct_player";
const REVENUE: &str = "acct_revenue";

/// (d) Opening a pack is deterministic per (receipt id + seed), and re-derives
/// byte-identically. The purchased pack's contents come only from its receipt.
#[tokio::test]
async fn pack_opening_is_deterministic_per_receipt_and_seed() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);

    let out = store
        .purchase(PLAYER, "pack.standard", "buy-1")
        .await
        .unwrap();
    let opened = store.open_pack(PLAYER, &out.entitlement_id, 7).unwrap();
    assert_eq!(opened.len(), 5);

    // Every drop is content-addressed.
    for item in &opened {
        assert!(item.item_id.starts_with("item_"));
    }

    // The same receipt id + seed re-derives exactly the same drops (audit).
    let recomputed = game_economy_core::derive_pack_contents(
        match store.catalog().get("pack.standard").unwrap() {
            game_economy_core::Product::CardPack(t) => t,
            _ => unreachable!(),
        },
        &store.catalog(),
        &out.receipt_id,
        7,
    );
    assert_eq!(opened, recomputed);

    // Each drop unlocked a cosmetic in the inventory.
    let inv = store.inventory(PLAYER);
    assert!(inv.packs.get(&out.entitlement_id).unwrap().is_opened());
    assert!(!inv.cosmetics.is_empty());
}

/// A pack opens exactly once; a second open is rejected.
#[tokio::test]
async fn pack_opens_exactly_once() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let out = store
        .purchase(PLAYER, "pack.standard", "buy-1")
        .await
        .unwrap();

    let first = store.open_pack(PLAYER, &out.entitlement_id, 1).unwrap();
    let err = store.open_pack(PLAYER, &out.entitlement_id, 1).unwrap_err();
    assert_eq!(
        err,
        GameError::PackAlreadyOpened(out.entitlement_id.clone())
    );
    assert_eq!(first.len(), 5);
}

/// A different seed yields different drops from the same receipt.
#[tokio::test]
async fn different_seed_changes_drops() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let out = store.purchase(PLAYER, "pack.mega", "buy-1").await.unwrap();

    let catalog = store.catalog();
    let tier = match catalog.get("pack.mega").unwrap() {
        game_economy_core::Product::CardPack(t) => t,
        _ => unreachable!(),
    };
    let a = game_economy_core::derive_pack_contents(tier, &catalog, &out.receipt_id, 1);
    let b = game_economy_core::derive_pack_contents(tier, &catalog, &out.receipt_id, 2);
    assert_ne!(a, b);
}

// --- FakeMint contract fidelity (mirrors real mint-store semantics) -----------

/// Capture is terminal: releasing a captured hold is `HoldNotFound`.
#[tokio::test]
async fn releasing_a_captured_hold_errors() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let hold = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: 1_000_000,
            idem_key: "k:hold".into(),
        })
        .await
        .unwrap();
    mint.capture_hold(
        &hold.hold_id,
        Capture {
            amount_micros: 1_000_000,
            to: REVENUE.into(),
            idem_key: "k:capture".into(),
        },
    )
    .await
    .unwrap();

    let err = mint
        .release_hold(
            &hold.hold_id,
            Release {
                idem_key: "k:release".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err, GameError::HoldNotFound);
}

/// Idempotent replay of a capture is a no-op returning the ORIGINAL receipt,
/// flagged `idempotent_replay`, moving no additional money.
#[tokio::test]
async fn capture_replay_is_a_noop() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let hold = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: 2_000_000,
            idem_key: "k:hold".into(),
        })
        .await
        .unwrap();
    let first = mint
        .capture_hold(
            &hold.hold_id,
            Capture {
                amount_micros: 2_000_000,
                to: REVENUE.into(),
                idem_key: "k:capture".into(),
            },
        )
        .await
        .unwrap();
    let replay = mint
        .capture_hold(
            &hold.hold_id,
            Capture {
                amount_micros: 2_000_000,
                to: REVENUE.into(),
                idem_key: "k:capture".into(),
            },
        )
        .await
        .unwrap();

    assert!(!first.idempotent_replay);
    assert!(replay.idempotent_replay);
    assert_eq!(first.receipt_id, replay.receipt_id);
    assert_eq!(first.settlement_seq, replay.settlement_seq);
    // Revenue credited exactly once.
    assert_eq!(mint.balance(REVENUE), 2_000_000);
}

/// Same idem_key with a different body is an idempotency conflict.
#[tokio::test]
async fn same_key_different_body_is_conflict() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    mint.create_hold(Hold {
        account_id: PLAYER.into(),
        amount_micros: 1_000_000,
        idem_key: "dup".into(),
    })
    .await
    .unwrap();
    let err = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: 5_000_000, // different amount, same key
            idem_key: "dup".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err, GameError::IdempotencyConflict);
}

/// Insufficient balance is a hard failure at hold time.
#[tokio::test]
async fn create_hold_over_balance_is_402() {
    let mint = FakeMint::new().with_balance(PLAYER, 500_000);
    let err = mint
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: 1_000_000,
            idem_key: "k".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err, GameError::InsufficientFunds);
    assert_eq!(mint.balance(PLAYER), 500_000);
}

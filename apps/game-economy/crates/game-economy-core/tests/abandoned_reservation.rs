//! A single-owner reservation left behind by a purchase that never finished
//! (its release did not land, or the process died) no longer blocks the
//! player forever: after the TTL, a new purchase proves on Mint that the old
//! one can never charge (its hold is released under the old key) and takes the
//! reservation over. A paid-but-not-granted old purchase keeps it.

use std::sync::Arc;
use std::time::Duration;

use game_economy_core::transport::{Capture, Hold, MintTransport};
use game_economy_core::{
    single_owner_key, Catalog, GameError, GameStateRepository, GameStore, MemoryGameState,
};
use game_economy_testkit::{Call, FakeMint};

const PLAYER: &str = "acct_player";
const REVENUE: &str = "acct_revenue";
const SKIN: &str = "skin.aeon.blade";

fn ownership_key() -> String {
    single_owner_key(Catalog::demo().get(SKIN).unwrap())
        .unwrap()
        .0
}

/// A purchase under `key` that reserved the skin and then stopped.
fn abandoned(state: &MemoryGameState, key: &str) {
    state.begin_purchase(key, PLAYER, SKIN).unwrap();
    state
        .reserve_single_owner(PLAYER, &ownership_key(), SKIN, key)
        .unwrap();
}

fn store(state: Arc<MemoryGameState>, ttl: Duration) -> GameStore<FakeMint> {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    GameStore::with_state(
        mint,
        game_economy_core::AlwaysFulfill,
        Catalog::demo(),
        REVENUE,
        state,
    )
    .with_reservation_ttl(ttl)
}

#[tokio::test]
async fn an_abandoned_reservation_is_taken_over_after_the_ttl() {
    let state = Arc::new(MemoryGameState::default());
    abandoned(&state, "old-key");
    let store = store(state.clone(), Duration::ZERO);

    let out = store
        .purchase(PLAYER, SKIN, "new-key")
        .await
        .expect("the player can buy again");
    assert_eq!(out.charged_micros, 4_990_000);
    // Charged once: the old key's hold was opened and released (net zero).
    assert_eq!(store.mint().balance(PLAYER), 10_000_000 - 4_990_000);
    assert_eq!(store.mint().balance(REVENUE), 4_990_000);
    let calls = store.mint().calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::Capture { .. }))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::Release { .. }))
            .count(),
        1
    );
    assert_eq!(
        state
            .single_owner_holder(PLAYER, &ownership_key())
            .unwrap()
            .unwrap()
            .idem_key,
        "new-key"
    );

    // The old purchase can no longer charge: its hold is released.
    let late = store
        .mint()
        .capture_hold(
            "hold_1",
            Capture {
                amount_micros: 4_990_000,
                to: REVENUE.into(),
                idem_key: "old-key:capture".into(),
            },
        )
        .await;
    assert!(matches!(late, Err(GameError::HoldNotFound)), "{late:?}");
}

#[tokio::test]
async fn a_recent_reservation_still_blocks() {
    let state = Arc::new(MemoryGameState::default());
    abandoned(&state, "old-key");
    let store = store(state, Duration::from_secs(900));
    let err = store.purchase(PLAYER, SKIN, "new-key").await.unwrap_err();
    assert!(matches!(err, GameError::AlreadyOwned(_)), "{err:?}");
    assert!(store.mint().calls().is_empty(), "nothing touched on Mint");
}

#[tokio::test]
async fn a_paid_old_purchase_keeps_the_reservation() {
    let state = Arc::new(MemoryGameState::default());
    abandoned(&state, "old-key");
    let store = store(state.clone(), Duration::ZERO);
    // The old purchase captured, then died before granting.
    let hold = store
        .mint()
        .create_hold(Hold {
            account_id: PLAYER.into(),
            amount_micros: 4_990_000,
            idem_key: "old-key:hold".into(),
        })
        .await
        .unwrap();
    store
        .mint()
        .capture_hold(
            &hold.hold_id,
            Capture {
                amount_micros: 4_990_000,
                to: REVENUE.into(),
                idem_key: "old-key:capture".into(),
            },
        )
        .await
        .unwrap();

    let err = store.purchase(PLAYER, SKIN, "new-key").await.unwrap_err();
    assert!(matches!(err, GameError::AlreadyOwned(_)), "{err:?}");
    assert_eq!(
        store.mint().balance(PLAYER),
        10_000_000 - 4_990_000,
        "charged once, not twice"
    );
    assert_eq!(
        state
            .single_owner_holder(PLAYER, &ownership_key())
            .unwrap()
            .unwrap()
            .idem_key,
        "old-key"
    );

    // The same-key retry completes the paid purchase (the existing recovery).
    let done = store
        .purchase(PLAYER, SKIN, "old-key")
        .await
        .expect("old purchase completes");
    assert!(store.inventory(PLAYER).cosmetics.len() == 1);
    assert_eq!(store.mint().balance(PLAYER), 10_000_000 - 4_990_000);
    assert_eq!(done.charged_micros, 4_990_000);
}

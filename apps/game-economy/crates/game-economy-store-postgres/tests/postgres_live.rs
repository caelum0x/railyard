//! Live Postgres tests of the game state adapter (it had none). Gated on
//! `GAME_DATABASE_URL` / `DATABASE_URL` (a scratch database): without it each
//! test returns early.
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p game-economy-store-postgres --test postgres_live
//! ```

use std::sync::Arc;

use game_economy_core::{GameError, GameStateRepository};
use game_economy_store_postgres::PostgresGameState;

async fn state() -> Option<Arc<PostgresGameState>> {
    let url = std::env::var("GAME_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    let s = PostgresGameState::connect(&url).await.expect("connect");
    s.migrate().await.expect("migrate");
    Some(Arc::new(s))
}

fn tag(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{name}-{nanos:x}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reservations_are_exclusive_and_move_only_from_their_holder() {
    let Some(s) = state().await else { return };
    let player = tag("p");
    let key = "cosmetic:x";
    s.begin_purchase("k1", &player, "skin.x").ok();
    s.reserve_single_owner(&player, key, "skin.x", &format!("{player}-k1"))
        .unwrap();
    // Idempotent for the holder, refused for anyone else.
    s.reserve_single_owner(&player, key, "skin.x", &format!("{player}-k1"))
        .unwrap();
    assert!(matches!(
        s.reserve_single_owner(&player, key, "skin.x", &format!("{player}-k2")),
        Err(GameError::AlreadyOwned(_))
    ));
    let holder = s.single_owner_holder(&player, key).unwrap().unwrap();
    assert_eq!(holder.idem_key, format!("{player}-k1"));
    assert!((0..60_000).contains(&holder.age_ms), "{}", holder.age_ms);

    // Only the current holder's reservation moves.
    assert!(!s
        .take_over_single_owner(&player, key, "not-the-holder", "k3")
        .unwrap());
    assert!(s
        .take_over_single_owner(
            &player,
            key,
            &format!("{player}-k1"),
            &format!("{player}-k2")
        )
        .unwrap());
    assert_eq!(
        s.single_owner_holder(&player, key)
            .unwrap()
            .unwrap()
            .idem_key,
        format!("{player}-k2")
    );
    s.release_single_owner(&player, key, &format!("{player}-k2"));
    assert!(s.single_owner_holder(&player, key).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_takeovers_have_one_winner() {
    let Some(s) = state().await else { return };
    let player = tag("race");
    let key = "pass:season.1";
    s.reserve_single_owner(&player, key, "pass.season.1", "old")
        .unwrap();
    let mut tasks = Vec::new();
    for i in 0..8 {
        let s = s.clone();
        let player = player.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            s.take_over_single_owner(&player, key, "old", &format!("new-{i}"))
                .unwrap()
        }));
    }
    let mut wins = 0;
    for t in tasks {
        if t.await.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(
        wins, 1,
        "exactly one purchase takes the abandoned reservation"
    );
}

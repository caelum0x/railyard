//! Hedged-dispatch race tests with FAKE transports.

mod common;

use std::time::Duration;

use common::{fake_attempt, Script};
use googletest::prelude::*;
use relay_hedge::{race, HedgeConfig, HedgeError};

#[gtest]
#[tokio::test(start_paused = true)]
async fn fast_provider_wins_the_race() {
    // A is slow-ok, B is fast-ok. With concurrency 2 the first token (B) wins.
    let (a, ha) = fake_attempt("A", 0, Duration::from_millis(500), Script::Ok);
    let (b, hb) = fake_attempt("B", 0, Duration::from_millis(10), Script::Ok);

    let out = race(vec![a, b], &HedgeConfig::hedged(2))
        .await
        .expect("a winner");

    expect_that!(out.winner.as_ref(), eq("B"));
    expect_that!(out.raced, eq(true));
    expect_that!(hb.completed(), eq(1));
    // A was launched but canceled when B won — it never produced a value.
    expect_that!(ha.completed(), eq(0));
    expect_that!(ha.dropped_incomplete(), eq(true));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn loser_future_is_canceled_when_winner_returns() {
    // The loser hangs "forever"; the fast winner must cancel it via future drop.
    let (hang, hh) = fake_attempt("hang", 0, Duration::from_secs(3600), Script::Ok);
    let (fast, hf) = fake_attempt("fast", 0, Duration::from_millis(5), Script::Ok);

    let out = race(vec![hang, fast], &HedgeConfig::hedged(2))
        .await
        .expect("a winner");

    expect_that!(out.winner.as_ref(), eq("fast"));
    expect_that!(hf.completed(), eq(1));
    // Drop sentinel fired and no response was produced by the loser.
    expect_that!(hh.dropped_incomplete(), eq(true));
    expect_that!(hh.completed(), eq(0));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn cost_cap_blocks_over_cap_hedges() {
    // Each attempt worst-cases 100 micros; a 150-micro cap fits only one at a
    // time, so B and C are never launched (the first is exempt so we still run).
    let (a, ha) = fake_attempt("A", 100, Duration::from_millis(10), Script::Ok);
    let (b, hb) = fake_attempt("B", 100, Duration::from_millis(10), Script::Ok);
    let (c, hc) = fake_attempt("C", 100, Duration::from_millis(10), Script::Ok);

    let cfg = HedgeConfig {
        enabled: true,
        max_concurrency: 3,
        stagger_ms: None,
        max_cost_micros: Some(150),
    };
    let out = race(vec![a, b, c], &cfg).await.expect("a winner");

    expect_that!(out.winner.as_ref(), eq("A"));
    expect_that!(out.launched, eq(1));
    expect_that!(ha.started(), eq(1));
    expect_that!(hb.started(), eq(0));
    expect_that!(hc.started(), eq(0));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn cost_cap_degrades_to_sequential_tail_on_failure() {
    // A fails fast, freeing its cost slot so B can then be launched sequentially.
    let (a, ha) = fake_attempt("A", 100, Duration::from_millis(10), Script::Err);
    let (b, hb) = fake_attempt("B", 100, Duration::from_millis(10), Script::Ok);

    let cfg = HedgeConfig {
        enabled: true,
        max_concurrency: 2,
        stagger_ms: None,
        max_cost_micros: Some(150),
    };
    let out = race(vec![a, b], &cfg).await.expect("a winner");

    expect_that!(out.winner.as_ref(), eq("B"));
    expect_that!(ha.started(), eq(1));
    expect_that!(hb.started(), eq(1));
}

#[gtest]
#[tokio::test]
async fn single_provider_default_takes_sequential_path() {
    // Default config (disabled, concurrency 1) => no raced branch.
    let (a, ha) = fake_attempt("A", 0, Duration::from_millis(1), Script::Ok);

    let out = race(vec![a], &HedgeConfig::default())
        .await
        .expect("a winner");

    expect_that!(out.winner.as_ref(), eq("A"));
    expect_that!(out.raced, eq(false));
    expect_that!(out.launched, eq(1));
    expect_that!(ha.completed(), eq(1));
}

#[gtest]
#[tokio::test]
async fn disabled_config_is_sequential_first_ok_wins() {
    // Two providers but default (disabled) config: A wins first-Ok and B is
    // NEVER launched — byte-for-byte the pre-existing one-at-a-time loop.
    let (a, _ha) = fake_attempt("A", 0, Duration::from_millis(1), Script::Ok);
    let (b, hb) = fake_attempt("B", 0, Duration::from_millis(1), Script::Ok);

    let out = race(vec![a, b], &HedgeConfig::default())
        .await
        .expect("a winner");

    expect_that!(out.winner.as_ref(), eq("A"));
    expect_that!(out.raced, eq(false));
    expect_that!(hb.started(), eq(0));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn all_attempts_fail_accumulates_errors() {
    let (a, _) = fake_attempt("A", 0, Duration::from_millis(1), Script::Err);
    let (b, _) = fake_attempt("B", 0, Duration::from_millis(1), Script::Err);

    let err = race(vec![a, b], &HedgeConfig::hedged(2))
        .await
        .err()
        .expect("all attempts failed");

    let HedgeError::AllFailed { errors } = err else {
        fail!("expected AllFailed, got {err:?}").expect("wrong error variant");
        unreachable!();
    };
    let names: Vec<String> = errors.iter().map(|(ep, _)| ep.to_string()).collect();
    expect_that!(names.len(), eq(2));
    expect_that!(names.contains(&"A".to_string()), eq(true));
    expect_that!(names.contains(&"B".to_string()), eq(true));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn streaming_first_chunk_wins_and_loser_stream_dropped() {
    // Model each streaming attempt's future as resolving at TTFT (first chunk).
    // B yields its first chunk first, so it wins; A's stream future is dropped
    // before it is ever handed to the caller (never a second stream).
    let (a, ha) = fake_attempt("A", 0, Duration::from_millis(300), Script::Ok);
    let (b, hb) = fake_attempt("B", 0, Duration::from_millis(20), Script::Ok);

    let out = race(vec![a, b], &HedgeConfig::hedged(2))
        .await
        .expect("a winning stream");

    expect_that!(out.winner.as_ref(), eq("B"));
    expect_that!(hb.completed(), eq(1));
    expect_that!(ha.completed(), eq(0));
    expect_that!(ha.dropped_incomplete(), eq(true));
}

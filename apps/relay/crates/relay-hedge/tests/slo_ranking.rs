//! SLO / health scorer + circuit-breaker ranking tests.

use std::sync::Arc;
use std::time::Duration;

use googletest::prelude::*;
use relay_hedge::{CircuitState, InMemoryProviderHealth, Outcome, ProviderHealth, SloConfig};

#[gtest]
#[tokio::test(start_paused = true)]
async fn deprioritizes_high_error_endpoint() {
    let health = InMemoryProviderHealth::with_defaults();
    let routing: Vec<Arc<str>> = vec!["A".into(), "B".into()];

    // A takes a burst of errors; B stays unknown (neutral/healthy).
    for _ in 0..5 {
        health.record("m", "A", Outcome::transient_error());
    }

    let ranked = health.rank("m", &routing);
    expect_that!(ranked[0].as_ref(), eq("B"));
    expect_that!(ranked[1].as_ref(), eq("A"));

    // A tripped the breaker and is shed to the tail (still present as fallback).
    expect_that!(
        health.circuit_state("m", "A"),
        some(matches_pattern!(CircuitState::Open { .. }))
    );
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn equal_scores_preserve_configured_order() {
    // No history for anyone => neutral scores => stable, configured order.
    let health = InMemoryProviderHealth::with_defaults();
    let routing: Vec<Arc<str>> = vec!["A".into(), "B".into(), "C".into()];
    let ranked = health.rank("m", &routing);
    expect_that!(
        ranked.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        eq(&vec!["A".to_string(), "B".to_string(), "C".to_string()])
    );
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn half_open_admits_single_probe_then_closes_on_success() {
    let cfg = SloConfig {
        open_cooldown: Duration::from_secs(10),
        ..SloConfig::default()
    };
    let health = InMemoryProviderHealth::new(cfg);
    let routing: Vec<Arc<str>> = vec!["A".into()];

    for _ in 0..5 {
        health.record("m", "A", Outcome::transient_error());
    }
    expect_that!(
        health.circuit_state("m", "A"),
        some(matches_pattern!(CircuitState::Open { .. }))
    );

    // Before cooldown elapses, ranking keeps it Open (shed).
    let _ = health.rank("m", &routing);
    expect_that!(
        health.circuit_state("m", "A"),
        some(matches_pattern!(CircuitState::Open { .. }))
    );

    // Advance the paused clock past the cooldown.
    tokio::time::advance(Duration::from_secs(11)).await;

    // First ranking admits exactly one probe: HalfOpen{ probing: true }.
    let _ = health.rank("m", &routing);
    expect_that!(
        health.circuit_state("m", "A"),
        some(eq(CircuitState::HalfOpen { probing: true }))
    );

    // A second ranking does NOT admit another probe while one is in flight.
    let _ = health.rank("m", &routing);
    expect_that!(
        health.circuit_state("m", "A"),
        some(eq(CircuitState::HalfOpen { probing: true }))
    );

    // The probe succeeds => circuit closes.
    health.record("m", "A", Outcome::success(Duration::from_millis(50)));
    expect_that!(
        health.circuit_state("m", "A"),
        some(eq(CircuitState::Closed))
    );
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn half_open_probe_failure_reopens_circuit() {
    let cfg = SloConfig {
        open_cooldown: Duration::from_secs(10),
        ..SloConfig::default()
    };
    let health = InMemoryProviderHealth::new(cfg);
    let routing: Vec<Arc<str>> = vec!["A".into()];

    for _ in 0..5 {
        health.record("m", "A", Outcome::transient_error());
    }
    tokio::time::advance(Duration::from_secs(11)).await;
    let _ = health.rank("m", &routing); // admit the probe

    // The probe fails => circuit re-opens.
    health.record("m", "A", Outcome::transient_error());
    expect_that!(
        health.circuit_state("m", "A"),
        some(matches_pattern!(CircuitState::Open { .. }))
    );
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn warm_start_seed_reflects_error_history_in_ranking() {
    let health = InMemoryProviderHealth::with_defaults();
    let routing: Vec<Arc<str>> = vec!["A".into(), "B".into()];

    // Simulate a Postgres warm-start: A had a high error fraction, B was clean.
    health.seed("m", "A", Duration::from_millis(200), 0.8, 20);
    health.seed("m", "B", Duration::from_millis(120), 0.0, 20);

    let ranked = health.rank("m", &routing);
    expect_that!(ranked[0].as_ref(), eq("B"));
}

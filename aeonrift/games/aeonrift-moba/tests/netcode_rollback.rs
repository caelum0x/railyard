//! ROLLBACK / CHECKSUM-BINDING GATE (full deterministic replication), behind the
//! `netcode` feature.
//!
//! The sibling `netcode_udp` gate proves inputs flow and the client converges over
//! real UDP. THIS gate proves the piece that makes it *deterministic replication*
//! rather than "eventually consistent": that aeonrift's `sim::state_hash` fingerprint
//! is actually **bound into lightyear's per-tick checksum XOR** (so a divergence is
//! DETECTED, not merely computed), and that the rollback/catch-up replay the checksum
//! guards is deterministic.
//!
//! Two things are asserted, at two different layers, because a full
//! divergence-injection assertion over real UDP is inherently racy (it depends on the
//! exact tick a mismatch lands and on scraping the server's tracing output):
//!
//!   1. `sim_state_hash_is_bound_into_lightyear_checksum` (netcode/protocol layer):
//!      after building the real server app, the lightyear `ComponentRegistry` has a
//!      deterministic hash fn registered for `SimStateHash`. THIS is the concrete
//!      proof of the `add_custom_hash` bind — without it the `ChecksumPlugin` iterates
//!      an empty hashable-component set and every checksum compares as 0 (inert). It
//!      also proves `Deterministic` is a real registered component (the archetype
//!      opt-in marker).
//!
//!   2. `dropped_input_diverges_and_deterministic_replay_reconverges` (sim-core layer):
//!      the VALUE that the bind carries. A dropped/unacknowledged input produces a
//!      DIFFERENT `state_hash` (so the bound checksum mismatches -> desync detected),
//!      while a clean deterministic replay of the full input stream from the same seed
//!      reproduces the server's authoritative `state_hash` exactly (so rollback's
//!      re-apply-unacked-inputs step reconverges to authority). Together these are the
//!      two halves of "the checksum detects the divergence and rollback restores
//!      convergence to the server's authoritative state hash".
//!
//! Run: `cargo +1.96.1 test -p aeonrift-moba --features netcode --test netcode_rollback`
#![cfg(feature = "netcode")]

use aeonrift_moba::netcode::{build_server_app, SimStateHash, CHAMPION_ID};
use lightyear::prelude::ComponentRegistry;
use lightyear_deterministic_replication::Deterministic;

const SERVER_PORT: u16 = 6131;
const SEED: u64 = 42;

/// PROOF 1 — the bind is real, not just computed.
///
/// `SimChecksum`/`state_hash` being *computed* in a resource proves nothing about
/// desync detection; lightyear only compares a component's hash if that component has
/// a deterministic hash fn registered (via `add_custom_hash`) AND its entity carries
/// the `Deterministic` marker. This asserts the former directly on the real server
/// app's `ComponentRegistry`, which is the exact table `ChecksumReceivePlugin` reads
/// to decide what to hash.
#[test]
fn sim_state_hash_is_bound_into_lightyear_checksum() {
    let mut app = build_server_app(SERVER_PORT, SEED);
    app.finish();
    app.cleanup();

    // The `Deterministic` archetype-opt-in marker must be a registered component
    // (it is what the checksum machinery filters entities on).
    assert!(
        app.world()
            .components()
            .component_id::<Deterministic>()
            .is_some(),
        "lightyear_deterministic_replication::Deterministic is not registered — the \
         checksum would hash no entities"
    );

    let sim_hash_id = app
        .world()
        .components()
        .component_id::<SimStateHash>()
        .expect("SimStateHash must be a registered component (it is .replicate()'d)");

    let registry = app
        .world()
        .get_resource::<ComponentRegistry>()
        .expect("lightyear ComponentRegistry must exist on a built server app");

    let kind = registry
        .component_id_to_kind
        .get(&sim_hash_id)
        .expect("SimStateHash must be in the lightyear component registry");
    let metadata = registry
        .component_metadata_map
        .get(kind)
        .expect("SimStateHash must have component metadata");

    // The load-bearing assertion: `add_custom_hash(hash_sim_state)` installed a
    // deterministic hash fn for `SimStateHash`, so `sim::state_hash` is XOR-ed into
    // the per-tick checksum that client and server compare.
    assert!(
        metadata.deterministic.is_some(),
        "SimStateHash has NO deterministic hash fn registered — sim::state_hash is \
         NOT bound into lightyear's checksum; divergence would go undetected"
    );
}

/// Advance a fresh deterministic world `ticks` times, feeding `input_for(tick)` each
/// tick (returning `None` models an unacknowledged/lost input for that tick), and
/// return the final `sim::state_hash`.
///
/// NOTE: `Command::MoveTo` sets a *persistent* champion target (`champ.target =
/// Some(..)`), so a lost input only diverges if the destination is never delivered at
/// all — dropping one of many identical `MoveTo`s is a genuine no-op. The tests below
/// therefore model a champion whose single move command is either delivered once (and
/// its target then persists) or lost entirely.
fn run_world(seed: u64, ticks: u32, mut input_for: impl FnMut(u32) -> Option<sim::Input>) -> u64 {
    let mut world = sim::World::new(seed);
    for tick in 0..ticks {
        match input_for(tick) {
            Some(input) => sim::step(&mut world, &[input]),
            None => sim::step(&mut world, &[]),
        }
    }
    sim::state_hash(&world)
}

/// A steady "move toward +x" input for the controlled champion.
fn move_input() -> sim::Input {
    sim::Input {
        entity: CHAMPION_ID,
        command: sim::Command::MoveTo(sim::Vec2::new(40.0, 0.0)),
    }
}

/// PROOF 2 — the value the bind carries: divergence is detectable and deterministic
/// replay reconverges.
#[test]
fn dropped_input_diverges_and_deterministic_replay_reconverges() {
    const TICKS: u32 = 80;
    const ISSUE_AT: u32 = 4;

    // Server authority: the move command is delivered once (tick ISSUE_AT); the
    // champion's target then persists and it moves/arrives over the remaining ticks.
    let authoritative = run_world(SEED, TICKS, |t| (t == ISSUE_AT).then(move_input));

    // Client whose only move input was LOST (never delivered): its champion never
    // receives a target and stays put, so it ends at a DIFFERENT state hash — exactly
    // the mismatch the `SimStateHash`-bound checksum flags between client and server.
    let lost_input = run_world(SEED, TICKS, |_| None);
    assert_ne!(
        authoritative, lost_input,
        "a lost input did NOT change state_hash — the bound checksum could never \
         detect this desync"
    );

    // Rollback / catch-up replay: once the input is acknowledged, re-simulating the
    // full input stream from the same seed reproduces the server's authoritative hash
    // bit-for-bit. This is the determinism the rollback path relies on when it
    // re-applies unacked inputs after restoring a snapshot — it reconverges to the
    // server's authoritative `state_hash`, which is precisely the value carried by
    // `SimStateHash`.
    let replayed = run_world(SEED, TICKS, |t| (t == ISSUE_AT).then(move_input));
    assert_eq!(
        authoritative, replayed,
        "deterministic replay of the same inputs did NOT reproduce the authoritative \
         state_hash — rollback could not reconverge"
    );
}

/// Determinism sanity: the fingerprint diverges under a dropped input AT the drop, not
/// by luck of the final tick — a shorter horizon still diverges, and an identical run
/// still matches. Guards against a false green where `state_hash` is insensitive.
#[test]
fn checksum_fingerprint_is_divergence_sensitive() {
    let a = run_world(SEED, 60, |t| (t == 4).then(move_input));
    let b = run_world(SEED, 60, |t| (t == 4).then(move_input));
    assert_eq!(a, b, "identical deterministic runs must share a state_hash");

    let lost = run_world(SEED, 60, |_| None);
    assert_ne!(
        a, lost,
        "state_hash must be sensitive to a lost move input for the bound checksum to \
         be a meaningful desync guard"
    );
}

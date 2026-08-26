//! Shared setup + the sim kernel driver that runs on both peers.
//!
//! `sim-core` stays the untouched deterministic kernel: lightyear decides *when*
//! `sim::step` runs (in `FixedUpdate`, at the fixed `DT` tick) and *what inputs* it
//! gets (the replicated [`MoveInput`]s); it never replaces the sim.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use core::time::Duration;

use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;

use sim::{state_hash, step, EntityId, Input, World};

use super::protocol::{MoveInput, NetChampion, ProtocolPlugin, SimStateHash};

/// The champion id every connected peer drives in this single-champion demo.
pub const CHAMPION_ID: EntityId = 0;

/// Loopback server address for the local/UDP demo + tests.
pub fn server_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// One `FixedUpdate` tick duration, taken from the sim's fixed rate so lightyear's
/// tick clock == the sim's `DT`.
pub fn tick_duration() -> Duration {
    Duration::from_secs_f64(1.0 / sim::TICK_HZ as f64)
}

/// The authoritative (server) or predicted (client) sim world, wrapped as a Bevy
/// resource. `sim::step` is the only thing that mutates it.
#[derive(Resource)]
pub struct SimNet {
    pub world: World,
}

impl SimNet {
    pub fn new(seed: u64) -> Self {
        SimNet {
            world: World::new(seed),
        }
    }
}

/// The per-tick `sim::state_hash` — aeonrift's determinism fingerprint. This is the
/// value lightyear's checksum protocol compares to detect desync (the
/// `no_desync` invariant becomes a live runtime guard).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimChecksum(pub u64);

/// Shared plugin: the protocol + the sim kernel driver + the checksum resource.
#[derive(Clone)]
pub struct SharedPlugin;

impl Plugin for SharedPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProtocolPlugin);
        app.init_resource::<SimChecksum>();
        // The sim kernel runs in FixedUpdate on both peers (server = authority,
        // client = prediction), driven by the tick's networked inputs.
        app.add_systems(FixedUpdate, sim_step_system);
    }
}

/// Collect this tick's `MoveInput`s off the champion entities, advance the shared
/// deterministic sim by exactly one tick, then mirror the champion's state back
/// into its replicated [`NetChampion`] component. Updates [`SimChecksum`].
fn sim_step_system(
    mut sim: ResMut<SimNet>,
    mut champs: Query<(&ActionState<MoveInput>, &mut NetChampion, &mut SimStateHash)>,
    mut checksum: ResMut<SimChecksum>,
) {
    // Build the deterministic input slice (champion 0 in this single-champ demo).
    let mut inputs: Vec<Input> = Vec::new();
    for (action, _, _) in &champs {
        if let Some(input) = action.0.to_sim(CHAMPION_ID) {
            inputs.push(input);
        }
    }

    // The untouched deterministic kernel advances the world one fixed tick.
    step(&mut sim.world, &inputs);
    let hash = state_hash(&sim.world);
    checksum.0 = hash;

    // Mirror authoritative champion state into the replicated component AND publish
    // the per-tick determinism fingerprint into the checksum-bound `SimStateHash`
    // component, so lightyear's per-tick checksum XOR is computed over `state_hash`
    // (on the server: the authoritative hash; on the client: the predicted hash that
    // lightyear records into `PredictionHistory<SimStateHash>` for the confirmed-tick
    // comparison).
    if let Some(c) = sim.world.champion(CHAMPION_ID) {
        for (_, mut net, mut sim_hash) in &mut champs {
            net.x = c.pos.x;
            net.y = c.pos.y;
            net.hp = c.health.current;
            net.team = c.team;
            sim_hash.0 = hash;
        }
    }
}

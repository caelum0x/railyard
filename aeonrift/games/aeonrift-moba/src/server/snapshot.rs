//! Snapshot broadcasting helpers.
//!
//! The authoritative [`super::Server`] already produces a full [`Snapshot`]. For
//! bandwidth telemetry and interest management this module computes a light,
//! deterministic *delta* between two snapshots: which champions changed position or
//! health, and which projectiles appeared/vanished. It never mutates state.

use sim::net::Snapshot;
use sim::EntityId;

/// A deterministic summary of what changed between two snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotDelta {
    /// Champions whose position or health changed (ascending id order).
    pub changed_champions: Vec<EntityId>,
    /// Champions that died between the two snapshots.
    pub newly_dead: Vec<EntityId>,
    /// Net change in the number of in-flight projectiles.
    pub projectile_delta: i64,
    /// Ticks elapsed between the two snapshots.
    pub tick_span: u64,
}

/// Compute the delta from `prev` to `next`. Both champion lists are iterated in
/// stable id order (the sim keeps them sorted), so the result is deterministic.
pub fn delta(prev: &Snapshot, next: &Snapshot) -> SnapshotDelta {
    let mut changed_champions = Vec::new();
    let mut newly_dead = Vec::new();

    for nc in &next.world.champions {
        match prev.world.champion(nc.id) {
            Some(pc) => {
                if pc.pos != nc.pos || pc.health != nc.health {
                    changed_champions.push(nc.id);
                }
                if pc.alive && !nc.alive {
                    newly_dead.push(nc.id);
                }
            }
            None => changed_champions.push(nc.id), // newly spawned
        }
    }

    SnapshotDelta {
        changed_champions,
        newly_dead,
        projectile_delta: next.world.projectiles.len() as i64
            - prev.world.projectiles.len() as i64,
        tick_span: next.world.tick.saturating_sub(prev.world.tick),
    }
}

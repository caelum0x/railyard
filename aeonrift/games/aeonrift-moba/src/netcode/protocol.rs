//! The shared lightyear protocol: networked inputs + replicated components.
//!
//! This is the wire contract that maps aeonrift's hand-rolled netcode spine onto
//! lightyear 0.29 (Bevy 0.19):
//!
//! | aeonrift spine (`sim::net`)      | lightyear                              |
//! |----------------------------------|----------------------------------------|
//! | `StampedInput` (inputs on wire)  | [`MoveInput`] native input (inputs-only)|
//! | `Client::predict` / `reconcile`  | `PredictionTarget` + rollback           |
//! | `sim::state_hash`                | `ChecksumPlugin` desync fingerprint     |
//! | fixed `DT` tick                  | `FixedUpdate` at `sim::TICK_HZ`         |
//! | `effect_tick` input delay        | lightyear input-delay config            |
//!
//! `sim-core` itself is untouched: these are serde/Bevy-facing newtypes that carry
//! the same information across the network and hand it to `sim::step`.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear_deterministic_replication::prelude::ChecksumPlugin;
use serde::{Deserialize, Serialize};

use sim::{Command, Input, Vec2};

/// The networked input for a champion — the wire form of a [`sim::Input`].
///
/// A `MoveInput` is inputs-only (no state), exactly like `StampedInput`: lightyear
/// buffers it per tick, applies input delay, and rebroadcasts it. On the sim side
/// it becomes a `Command::MoveTo` (or `CastSkillshot` when `cast` is set).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Reflect)]
pub struct MoveInput {
    pub x: f32,
    pub y: f32,
    /// When true, the input is a skillshot cast toward `(x, y)` instead of a move.
    pub cast: bool,
    /// When false, the champion holds position this tick (no command emitted).
    pub active: bool,
}

impl Default for MoveInput {
    fn default() -> Self {
        MoveInput {
            x: 0.0,
            y: 0.0,
            cast: false,
            active: false,
        }
    }
}

impl MoveInput {
    /// A click-to-move input toward `target`.
    pub fn move_to(target: Vec2) -> Self {
        MoveInput {
            x: target.x,
            y: target.y,
            cast: false,
            active: true,
        }
    }

    /// Convert to a `sim::Input` for champion `entity`, if this input is active.
    pub fn to_sim(self, entity: sim::EntityId) -> Option<Input> {
        if !self.active {
            return None;
        }
        let aim = Vec2::new(self.x, self.y);
        let command = if self.cast {
            Command::CastSkillshot(aim)
        } else {
            Command::MoveTo(aim)
        };
        Some(Input { entity, command })
    }
}

// Native inputs must be entity-mappable; a `MoveInput` carries no entity refs.
impl bevy::ecs::entity::MapEntities for MoveInput {
    fn map_entities<M: bevy::ecs::entity::EntityMapper>(&mut self, _m: &mut M) {}
}

/// A replicated snapshot of a champion's authoritative state. The server writes it
/// from `sim::step`; lightyear replicates + predicts it to clients.
#[derive(Component, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Reflect)]
pub struct NetChampion {
    pub x: f32,
    pub y: f32,
    pub hp: i32,
    pub team: u8,
}

impl NetChampion {
    pub fn pos(&self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

// Linear interpolation so lightyear can smoothly interpolate remote champions.
impl Ease for NetChampion {
    fn interpolating_curve_unbounded(start: Self, end: Self) -> impl Curve<Self> {
        FunctionCurve::new(Interval::UNIT, move |t| NetChampion {
            x: start.x + (end.x - start.x) * t,
            y: start.y + (end.y - start.y) * t,
            hp: end.hp,
            team: end.team,
        })
    }
}

/// Marks the entity that carries a networked champion and names its owning peer.
#[derive(Component, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Reflect)]
pub struct NetPlayer(pub u64);

/// The lightyear protocol shared by client and server.
#[derive(Clone)]
pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        // Inputs-only wire form (maps to `StampedInput`).
        app.add_plugins(input::native::InputPlugin::<MoveInput>::default());

        // Per-tick desync fingerprint. Our authoritative fingerprint is
        // `sim::state_hash` (see `shared::SimChecksum`); the ChecksumPlugin gives
        // lightyear the machinery to compare it across peers.
        app.add_plugins(ChecksumPlugin);

        // Replicated + predicted authoritative champion snapshot.
        app.component::<NetChampion>()
            .replicate()
            .predict()
            .add_linear_interpolation();
        app.component::<NetPlayer>().replicate();
    }
}

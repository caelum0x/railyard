//! Client-side, NON-authoritative navmesh via `vleue_navigator` (Polyanya).
//!
//! This is view-layer only: it builds an any-angle navmesh over the arena for
//! smooth path *previews* / minimap routes / movement-order visualization on the
//! Bevy 0.19 client. It NEVER feeds the deterministic sim — authoritative pathing
//! is `aeonrift-pathfinding` (integer flow-field, Bevy-free). Keeping the two
//! separate is what lets vleue's float-based Polyanya be used at all without
//! endangering the determinism contract.
//!
//! Gated behind the `client` feature so the default (Bevy-0.20-dev) workspace build
//! never pulls Bevy 0.19 / vleue.

use bevy::math::Vec2;
use bevy::prelude::*;
use vleue_navigator::{NavMesh, VleueNavigatorPlugin};

/// The client-side arena navmesh, used only for visualization / path preview.
#[derive(Resource)]
pub struct ClientNavMesh(pub NavMesh);

/// Adds vleue's navmesh support and bakes an arena navmesh for the client.
pub struct ClientNavPlugin {
    /// Half-extent of the square arena (matches `sim::MAP_BOUNDS` / the map bounds).
    pub bounds: f32,
    /// Circular obstacle footprints (center, radius) to cut out of the navmesh,
    /// e.g. structures — a purely cosmetic mirror of the sim's blockers.
    pub obstacles: Vec<(Vec2, f32)>,
}

impl Plugin for ClientNavPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(VleueNavigatorPlugin);
        let obstacles: Vec<Vec<Vec2>> = self
            .obstacles
            .iter()
            .map(|(c, r)| disc_polygon(*c, *r, 8))
            .collect();
        let navmesh = build_arena_navmesh(self.bounds, obstacles);
        app.insert_resource(ClientNavMesh(navmesh));
    }
}

/// Build an any-angle navmesh over the square arena `[-bounds, bounds]^2` with the
/// given obstacle polygons cut out.
pub fn build_arena_navmesh(bounds: f32, obstacles: Vec<Vec<Vec2>>) -> NavMesh {
    let outer = vec![
        Vec2::new(-bounds, -bounds),
        Vec2::new(bounds, -bounds),
        Vec2::new(bounds, bounds),
        Vec2::new(-bounds, bounds),
    ];
    NavMesh::from_edge_and_obstacles(outer, obstacles)
}

/// Whether the client navmesh can route between two points (path preview probe).
pub fn client_path_exists(nav: &ClientNavMesh, from: Vec2, to: Vec2) -> bool {
    nav.0.path(from, to).is_some()
}

/// An `n`-gon approximating a disc obstacle (client-side cosmetic footprint).
fn disc_polygon(center: Vec2, radius: f32, n: usize) -> Vec<Vec2> {
    (0..n.max(3))
        .map(|i| {
            let a = core::f32::consts::TAU * (i as f32) / (n.max(3) as f32);
            center + Vec2::new(radius * a.cos(), radius * a.sin())
        })
        .collect()
}

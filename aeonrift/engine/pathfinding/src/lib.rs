//! # aeonrift-pathfinding
//!
//! ROLE: Deterministic navmesh/flow-field pathfinding over the sim grid.
//!
//! This crate is the AUTHORITATIVE pathfinding layer: it is Bevy-free, has no
//! floating-point nondeterminism in its graph search (integer cells + unit-cost
//! BFS), and depends only on [`sim`] for the [`sim::Vec2`] boundary type. It
//! produces move targets that are fed straight into `sim::Input`, so lane/minion
//! movement runs through the exact same deterministic combat/movement core as
//! everything else.
//!
//! Client-side, any-angle NAVMESH visualization (vleue_navigator, Bevy 0.19) is a
//! separate, non-authoritative concern living behind the games' `client` feature;
//! it never feeds the sim. See `games/aeonrift-moba` `client` module.
//!
//! ## Pieces
//! - [`NavGrid`] — a uniform occupancy grid over `[-bounds, bounds]^2`.
//! - [`FlowField`] — a goal-directed BFS distance field; many agents descend one.
//! - [`line_clear`] — integer supercover line-of-sight for the direct-path shortcut.
//! - [`route`] — the high-level steer: LoS shortcut, else flow-field descent.
//!
//! ## Determinism contract
//! Same `(grid, goal, from)` => same result, byte-for-byte, matching the sim's
//! fixed-`f32` discipline (floats only at the world<->cell boundary).

pub mod flowfield;
pub mod grid;
pub mod los;

pub use flowfield::FlowField;
pub use grid::{Cell, NavGrid};
pub use los::line_clear;

use sim::Vec2;

/// The immediate world-space point an agent at `from` should walk toward to reach
/// `goal`, routing around obstacles in `grid`.
///
/// Fast path: if the straight segment `from -> goal` is unobstructed, return
/// `goal` unchanged (so an obstacle-free map behaves exactly like a straight-line
/// follow — zero behavioural change for existing content). Otherwise build a
/// one-shot [`FlowField`] to `goal` and return the next descent step; if the goal
/// is unreachable, fall back to `goal` (best-effort, never stalls the caller).
///
/// For many agents heading to the same goal, prefer building one [`FlowField`]
/// with [`FlowField::build`] and calling [`FlowField::next_step`] per agent —
/// `route` rebuilds the field each call and is meant for the one-off/LoS-clear
/// case.
pub fn route(grid: &NavGrid, from: Vec2, goal: Vec2) -> Vec2 {
    if line_clear(grid, from, goal) {
        return goal;
    }
    let field = FlowField::build(grid, goal);
    field.next_step(grid, from).unwrap_or(goal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_grid() -> NavGrid {
        NavGrid::new(50.0, 2.0)
    }

    #[test]
    fn open_map_route_is_the_direct_goal() {
        // No obstacles => LoS always clear => route returns the exact goal,
        // preserving straight-line follow byte-for-byte.
        let g = open_grid();
        let from = Vec2::new(-40.0, -40.0);
        let goal = Vec2::new(40.0, 40.0);
        assert_eq!(route(&g, from, goal), goal);
    }

    #[test]
    fn line_of_sight_detects_a_wall() {
        let mut g = NavGrid::new(20.0, 1.0);
        // Vertical wall at x≈0 spanning the whole height.
        for y in -20..20 {
            g.block_at(Vec2::new(0.0, y as f32 + 0.5));
        }
        assert!(!line_clear(&g, Vec2::new(-10.0, 0.0), Vec2::new(10.0, 0.0)));
        // A path that stays on one side is clear.
        assert!(line_clear(&g, Vec2::new(-10.0, 0.0), Vec2::new(-2.0, 0.0)));
    }

    #[test]
    fn flow_field_routes_around_a_wall_with_a_gap() {
        let mut g = NavGrid::new(20.0, 1.0);
        // Wall across x=0 from y=-20 up to y=5, leaving a gap near the top.
        for y in -20..5 {
            g.block_at(Vec2::new(0.0, y as f32 + 0.5));
        }
        let goal = Vec2::new(10.0, 0.0);
        let field = FlowField::build(&g, goal);
        let start = Vec2::new(-10.0, 0.0);
        assert!(field.reachable(&g, start), "goal must be reachable via the gap");

        // Walk the field and assert we actually arrive without crossing the wall.
        let mut pos = start;
        let mut steps = 0;
        loop {
            let next = field.next_step(&g, pos).expect("reachable => Some");
            assert!(!g.is_blocked(g.cell_of(next)), "never step onto a blocked cell");
            if next.distance(goal) < 1.5 || pos.distance(next) < 1e-6 {
                pos = next;
                break;
            }
            pos = next;
            steps += 1;
            assert!(steps < 500, "flow-field descent should terminate");
        }
        assert!(pos.distance(goal) < 2.0, "arrived near the goal, got {pos:?}");
    }

    #[test]
    fn unreachable_goal_reports_none() {
        let mut g = NavGrid::new(10.0, 1.0);
        // Fully wall off the goal region.
        let goal = Vec2::new(8.0, 8.0);
        for y in -10..10 {
            g.block_at(Vec2::new(5.0, y as f32 + 0.5));
        }
        for x in 5..10 {
            g.block_at(Vec2::new(x as f32 + 0.5, 5.0));
        }
        let field = FlowField::build(&g, goal);
        assert!(!field.reachable(&g, Vec2::new(-8.0, -8.0)));
        // route() must not panic and falls back to the goal.
        let _ = route(&g, Vec2::new(-8.0, -8.0), goal);
    }

    #[test]
    fn pathfinding_is_deterministic_across_runs() {
        // Two independently-built fields over the same obstacle layout must agree
        // step-for-step from every probe point.
        let build = || {
            let mut g = NavGrid::new(30.0, 1.5);
            g.block_disc(Vec2::new(0.0, 0.0), 6.0);
            g.block_disc(Vec2::new(10.0, -8.0), 4.0);
            g
        };
        let g1 = build();
        let g2 = build();
        assert_eq!(g1, g2, "grid construction is deterministic");

        let goal = Vec2::new(20.0, 20.0);
        let f1 = FlowField::build(&g1, goal);
        let f2 = FlowField::build(&g2, goal);
        assert_eq!(f1, f2, "flow field is deterministic");

        for &probe in &[
            Vec2::new(-25.0, -25.0),
            Vec2::new(-5.0, 12.0),
            Vec2::new(14.0, -2.0),
        ] {
            assert_eq!(
                f1.next_step(&g1, probe),
                f2.next_step(&g2, probe),
                "next_step diverged at {probe:?}"
            );
        }
    }

    #[test]
    fn block_disc_marks_expected_footprint() {
        let mut g = NavGrid::new(10.0, 1.0);
        g.block_disc(Vec2::new(0.0, 0.0), 2.0);
        assert!(g.is_blocked(g.cell_of(Vec2::new(0.0, 0.0))));
        assert!(g.is_blocked(g.cell_of(Vec2::new(1.5, 0.0))));
        assert!(!g.is_blocked(g.cell_of(Vec2::new(9.0, 9.0))));
    }
}

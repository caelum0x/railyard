//! DETERMINISTIC PATHFINDING GATE.
//!
//! The authoritative sim must stay deterministic and Bevy-free, so pathfinding is
//! a pure grid/flow-field layer (`aeonrift-pathfinding`) whose move targets feed
//! straight into `sim::Input`. This gate proves three things the MOBA relies on:
//!
//! 1. On an obstacle-free grid, routing returns the exact target — so wiring the
//!    nav grid into minion steering changes nothing for existing content.
//! 2. With a wall baked into the grid, an agent routes AROUND it (through a gap)
//!    and reaches the goal without ever stepping on a blocked cell.
//! 3. The route is byte-reproducible across independent runs (determinism).

use aeonrift_pathfinding::{route, FlowField, NavGrid};
use sim::Vec2;

#[test]
fn obstacle_free_route_is_the_direct_target() {
    let grid = NavGrid::new(50.0, 2.0);
    let from = Vec2::new(-43.0, -43.0);
    let goal = Vec2::new(43.0, 43.0);
    // No blockers => straight-line follow, unchanged from the pre-pathfinding path.
    assert_eq!(route(&grid, from, goal), goal);
}

#[test]
fn agent_routes_around_a_baked_wall() {
    let mut grid = NavGrid::new(30.0, 1.0);
    // A vertical wall at x≈0 from the bottom up to y=+8, leaving a gap above it.
    for y in -30..8 {
        grid.block_at(Vec2::new(0.0, y as f32 + 0.5));
    }
    let goal = Vec2::new(12.0, 0.0);
    let field = FlowField::build(&grid, goal);
    let start = Vec2::new(-12.0, 0.0);
    assert!(field.reachable(&grid, start), "the gap must make the goal reachable");

    // Descend the field to the goal; assert we cross the wall's x only above the gap.
    let mut pos = start;
    let mut crossed_above_gap = false;
    for _ in 0..1000 {
        let next = field.next_step(&grid, pos).expect("reachable => Some");
        assert!(
            !grid.is_blocked(grid.cell_of(next)),
            "must never route onto a blocked cell (next={next:?})"
        );
        // Detect the moment we move from the left side to the right side of x=0.
        if pos.x < 0.0 && next.x >= 0.0 {
            assert!(next.y >= 7.0, "crossing must happen through the top gap, got y={}", next.y);
            crossed_above_gap = true;
        }
        if next.distance(goal) < 1.5 || pos.distance(next) < 1e-6 {
            pos = next;
            break;
        }
        pos = next;
    }
    assert!(crossed_above_gap, "agent should have detoured through the gap");
    assert!(pos.distance(goal) < 2.0, "agent arrived near the goal, ended at {pos:?}");
}

#[test]
fn routing_is_deterministic() {
    let build = || {
        let mut g = NavGrid::new(40.0, 2.0);
        g.block_disc(Vec2::new(0.0, 0.0), 8.0);
        g.block_disc(Vec2::new(-12.0, 10.0), 5.0);
        g
    };
    let g1 = build();
    let g2 = build();
    let goal = Vec2::new(30.0, 30.0);
    let f1 = FlowField::build(&g1, goal);
    let f2 = FlowField::build(&g2, goal);
    assert_eq!(f1, f2, "flow field must be byte-identical across runs");

    for &p in &[
        Vec2::new(-35.0, -35.0),
        Vec2::new(-20.0, 20.0),
        Vec2::new(15.0, -10.0),
    ] {
        assert_eq!(
            f1.next_step(&g1, p),
            f2.next_step(&g2, p),
            "next step diverged at {p:?}"
        );
        assert_eq!(route(&g1, p, goal), route(&g2, p, goal));
    }
}

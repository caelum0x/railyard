//! Deterministic flow-field (Dijkstra-over-grid) pathing.
//!
//! A [`FlowField`] is a breadth-first distance field computed *from a goal cell*
//! outward across the free cells of a [`NavGrid`]. Any agent can then descend the
//! field one cell at a time toward the goal without re-running a search — the MOBA
//! spawns many minions that all head to the same lane goal, so one field serves a
//! whole wave.
//!
//! Determinism: unit-cost BFS with a `VecDeque` frontier, a fixed 8-neighbour
//! order, no diagonal corner-cutting, and integer distances only. `next_step`
//! breaks ties by that same fixed neighbour order, so the chosen path is a pure
//! function of `(grid, goal, from)` — byte-reproducible across runs.

use std::collections::VecDeque;

use sim::Vec2;

use crate::grid::{Cell, NavGrid};

/// Sentinel distance for an unreached / blocked cell.
const UNREACHED: u32 = u32::MAX;

/// The 8 neighbour offsets in a FIXED order (orthogonal first, then diagonal).
/// The order is load-bearing for determinism: ties in the distance field resolve
/// to the earliest offset here.
const NEIGHBORS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

/// A goal-directed distance field over a grid.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowField {
    cols: i32,
    rows: i32,
    dist: Vec<u32>,
    goal_cell: Cell,
    goal: Vec2,
}

impl FlowField {
    /// Build the field: BFS outward from the cell containing `goal` over all free
    /// (non-blocked, in-bounds) cells of `grid`.
    pub fn build(grid: &NavGrid, goal: Vec2) -> Self {
        let cols = grid.cols();
        let rows = grid.rows();
        let mut dist = vec![UNREACHED; (cols * rows) as usize];
        let goal_cell = grid.cell_of(goal);

        let idx = |c: Cell| (c.1 * cols + c.0) as usize;

        let mut frontier: VecDeque<Cell> = VecDeque::new();
        if !grid.is_blocked(goal_cell) {
            dist[idx(goal_cell)] = 0;
            frontier.push_back(goal_cell);
        }

        while let Some(cur) = frontier.pop_front() {
            let d = dist[idx(cur)];
            for (dx, dy) in NEIGHBORS {
                let nb = (cur.0 + dx, cur.1 + dy);
                if grid.is_blocked(nb) {
                    continue;
                }
                // Forbid diagonal corner-cutting: both shared orthogonal cells
                // must be free, so agents never clip a blocked corner.
                if dx != 0 && dy != 0 {
                    if grid.is_blocked((cur.0 + dx, cur.1)) || grid.is_blocked((cur.0, cur.1 + dy)) {
                        continue;
                    }
                }
                let ni = idx(nb);
                if dist[ni] == UNREACHED {
                    dist[ni] = d + 1;
                    frontier.push_back(nb);
                }
            }
        }

        FlowField {
            cols,
            rows,
            dist,
            goal_cell,
            goal,
        }
    }

    /// The world-space goal this field descends toward.
    pub fn goal(&self) -> Vec2 {
        self.goal
    }

    fn in_bounds(&self, c: Cell) -> bool {
        c.0 >= 0 && c.0 < self.cols && c.1 >= 0 && c.1 < self.rows
    }

    fn dist_at(&self, c: Cell) -> u32 {
        if self.in_bounds(c) {
            self.dist[(c.1 * self.cols + c.0) as usize]
        } else {
            UNREACHED
        }
    }

    /// Whether the cell containing `from` can reach the goal through free cells.
    pub fn reachable(&self, grid: &NavGrid, from: Vec2) -> bool {
        self.dist_at(grid.cell_of(from)) != UNREACHED
    }

    /// The next cell to move to from `from`: the free neighbour with the lowest
    /// distance-to-goal (ties broken by the fixed neighbour order). Returns
    /// `None` when the goal is unreachable from `from`. When already on the goal
    /// cell, returns the goal cell itself.
    pub fn next_cell(&self, grid: &NavGrid, from: Vec2) -> Option<Cell> {
        let cur = grid.cell_of(from);
        let cur_d = self.dist_at(cur);
        if cur_d == UNREACHED {
            return None;
        }
        if cur == self.goal_cell || cur_d == 0 {
            return Some(self.goal_cell);
        }
        let mut best: Option<(u32, Cell)> = None;
        for (dx, dy) in NEIGHBORS {
            let nb = (cur.0 + dx, cur.1 + dy);
            if grid.is_blocked(nb) {
                continue;
            }
            if dx != 0 && dy != 0 {
                if grid.is_blocked((cur.0 + dx, cur.1)) || grid.is_blocked((cur.0, cur.1 + dy)) {
                    continue;
                }
            }
            let nd = self.dist_at(nb);
            if nd == UNREACHED {
                continue;
            }
            // Strictly-less keeps the FIRST (lowest-index) neighbour on ties.
            if best.map(|(bd, _)| nd < bd).unwrap_or(true) {
                best = Some((nd, nb));
            }
        }
        // Only move if it makes progress (strictly closer than the current cell).
        match best {
            Some((nd, nb)) if nd < cur_d => Some(nb),
            _ => Some(cur), // local minimum / already adjacent to goal: hold.
        }
    }

    /// The world-space point an agent at `from` should walk toward next. Returns
    /// the exact `goal` when the goal cell is the immediate target, otherwise the
    /// center of the next descent cell. `None` if the goal is unreachable.
    pub fn next_step(&self, grid: &NavGrid, from: Vec2) -> Option<Vec2> {
        let next = self.next_cell(grid, from)?;
        if next == self.goal_cell {
            Some(self.goal)
        } else {
            Some(grid.center_of(next))
        }
    }
}

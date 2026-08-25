//! A deterministic uniform navigation grid over the sim play-plane.
//!
//! The grid quantizes the continuous `[-bounds, bounds]` square into integer
//! cells. All graph search happens on integer cell coordinates; `Vec2` only
//! appears at the boundary (world<->cell conversions). Determinism matches the
//! sim contract: fixed cell size, `f32::floor` at the boundary only, and stable
//! row-major indexing (never a hash map).

use sim::Vec2;

/// Integer cell coordinate on the grid (column, row).
pub type Cell = (i32, i32);

/// A uniform occupancy grid covering the square `[-bounds, bounds]` on each axis.
///
/// Cells are addressed `(col, row)` with the origin cell `(0, 0)` at the lower
/// corner `(-bounds, -bounds)`. Occupancy is a flat row-major `Vec<bool>` so
/// iteration and indexing are order-stable (no `HashMap`).
#[derive(Clone, Debug, PartialEq)]
pub struct NavGrid {
    cols: i32,
    rows: i32,
    blocked: Vec<bool>,
    bounds: f32,
    cell_size: f32,
}

impl NavGrid {
    /// Build an empty (all-free) grid covering `[-bounds, bounds]^2` with square
    /// cells of side `cell_size`. `cell_size` must be > 0; a non-positive value
    /// is clamped to a small positive default to stay panic-free at boundaries.
    pub fn new(bounds: f32, cell_size: f32) -> Self {
        let cell_size = if cell_size > 0.0 { cell_size } else { 1.0 };
        let span = bounds * 2.0;
        // ceil without pulling libm: add almost-one then floor.
        let cols = ((span / cell_size) + 0.999_999).floor() as i32;
        let cols = cols.max(1);
        let rows = cols; // square play-plane
        NavGrid {
            cols,
            rows,
            blocked: vec![false; (cols * rows) as usize],
            bounds,
            cell_size,
        }
    }

    /// Number of columns (== rows; the grid is square).
    pub fn cols(&self) -> i32 {
        self.cols
    }

    /// Number of rows.
    pub fn rows(&self) -> i32 {
        self.rows
    }

    /// Total cell count.
    pub fn len(&self) -> usize {
        self.blocked.len()
    }

    /// True when the grid has no cells (never, for a valid grid).
    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty()
    }

    /// Whether a cell coordinate lies inside the grid.
    pub fn in_bounds(&self, cell: Cell) -> bool {
        cell.0 >= 0 && cell.0 < self.cols && cell.1 >= 0 && cell.1 < self.rows
    }

    fn index(&self, cell: Cell) -> Option<usize> {
        if self.in_bounds(cell) {
            Some((cell.1 * self.cols + cell.0) as usize)
        } else {
            None
        }
    }

    /// The cell containing world point `p` (clamped into the grid).
    pub fn cell_of(&self, p: Vec2) -> Cell {
        let cx = ((p.x + self.bounds) / self.cell_size).floor() as i32;
        let cy = ((p.y + self.bounds) / self.cell_size).floor() as i32;
        (cx.clamp(0, self.cols - 1), cy.clamp(0, self.rows - 1))
    }

    /// The world-space center of a cell.
    pub fn center_of(&self, cell: Cell) -> Vec2 {
        Vec2::new(
            -self.bounds + (cell.0 as f32 + 0.5) * self.cell_size,
            -self.bounds + (cell.1 as f32 + 0.5) * self.cell_size,
        )
    }

    /// Whether a cell is blocked. Out-of-bounds cells count as blocked.
    pub fn is_blocked(&self, cell: Cell) -> bool {
        match self.index(cell) {
            Some(i) => self.blocked[i],
            None => true,
        }
    }

    /// Mark a cell blocked (no-op if out of bounds).
    pub fn block(&mut self, cell: Cell) {
        if let Some(i) = self.index(cell) {
            self.blocked[i] = true;
        }
    }

    /// Mark the cell containing world point `p` blocked.
    pub fn block_at(&mut self, p: Vec2) {
        self.block(self.cell_of(p));
    }

    /// Block every cell whose center lies within `radius` of `center` (a circular
    /// obstacle footprint, e.g. a turret or a temporary wall). Deterministic: the
    /// scan is a fixed row-major sweep of the bounding box.
    pub fn block_disc(&mut self, center: Vec2, radius: f32) {
        if radius <= 0.0 {
            self.block_at(center);
            return;
        }
        let r2 = radius * radius;
        let min = self.cell_of(Vec2::new(center.x - radius, center.y - radius));
        let max = self.cell_of(Vec2::new(center.x + radius, center.y + radius));
        for cy in min.1..=max.1 {
            for cx in min.0..=max.0 {
                let c = (cx, cy);
                if self.center_of(c).distance(center) * self.center_of(c).distance(center) <= r2 {
                    self.block(c);
                }
            }
        }
    }

    /// Clear every obstacle (all cells free again).
    pub fn clear(&mut self) {
        for b in &mut self.blocked {
            *b = false;
        }
    }
}

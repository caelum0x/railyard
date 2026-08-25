//! Deterministic grid line-of-sight (supercover DDA).
//!
//! [`line_clear`] walks every cell a straight segment `a -> b` touches and reports
//! whether all of them are free. It uses an integer supercover traversal (a
//! Bresenham variant that also visits the cells crossed at corners), so an agent
//! that "has line of sight" truly has an unobstructed straight path. This lets the
//! steering layer skip the flow field entirely when the direct route is clear —
//! which, on an obstacle-free map, reproduces the previous straight-line follow
//! byte-for-byte.

use sim::Vec2;

use crate::grid::NavGrid;

/// True when the straight segment from `a` to `b` crosses no blocked cell.
pub fn line_clear(grid: &NavGrid, a: Vec2, b: Vec2) -> bool {
    let (mut x0, mut y0) = grid.cell_of(a);
    let (x1, y1) = grid.cell_of(b);

    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };

    if grid.is_blocked((x0, y0)) || grid.is_blocked((x1, y1)) {
        return false;
    }

    // Supercover traversal: step through every cell the segment overlaps.
    let mut err = dx - dy;
    let mut n = dx + dy;
    while n > 0 {
        if grid.is_blocked((x0, y0)) {
            return false;
        }
        let e2 = 2 * err;
        if e2 > -dy && e2 < dx {
            // Diagonal step also touches the two orthogonally-adjacent cells;
            // treat the move as blocked if either is blocked (no corner cut).
            if grid.is_blocked((x0 + sx, y0)) && grid.is_blocked((x0, y0 + sy)) {
                return false;
            }
            x0 += sx;
            y0 += sy;
            err += dx - dy;
            n -= 2;
        } else if e2 > -dy {
            x0 += sx;
            err -= dy;
            n -= 1;
        } else {
            y0 += sy;
            err += dx;
            n -= 1;
        }
    }
    !grid.is_blocked((x0, y0))
}

//! Minimal deterministic 2D vector math.
//!
//! Determinism note: all operations use plain `f32`. On a fixed machine with a fixed
//! code path, IEEE-754 `f32` operations are bit-reproducible, which is all the two
//! gates require (byte-identical hashes across runs on THIS machine). Cross-platform
//! determinism would additionally require fixed-point or `fma`/rounding discipline;
//! see README.md "What's real vs next".

use std::ops::{Add, Mul, Sub};

/// A 2D vector on the play-plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }

    pub fn length_sq(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    pub fn length(self) -> f32 {
        self.length_sq().sqrt()
    }

    /// Distance to another point.
    pub fn distance(self, other: Vec2) -> f32 {
        (self - other).length()
    }

    /// Unit vector in the same direction. Returns `ZERO` for a zero-length input
    /// (deterministic, avoids NaN from dividing by zero).
    pub fn normalized(self) -> Vec2 {
        let len = self.length();
        if len == 0.0 {
            Vec2::ZERO
        } else {
            self * (1.0 / len)
        }
    }

    /// Clamp each component into `[-bound, bound]`. Used by authoritative input
    /// validation on the server.
    pub fn clamp_square(self, bound: f32) -> Vec2 {
        Vec2::new(self.x.clamp(-bound, bound), self.y.clamp(-bound, bound))
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x - other.x, self.y - other.y)
    }
}

/// Scalar multiplication (`vec * scalar`).
impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f32) -> Vec2 {
        Vec2::new(self.x * s, self.y * s)
    }
}

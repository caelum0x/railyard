//! Deterministic seeded RNG (SplitMix64).
//!
//! Hand-rolled to keep the sim dependency-free and 100% reproducible. Same seed +
//! same call sequence always yields the same stream, on any machine. The RNG state
//! is part of the world's hashed state so any accidental nondeterministic RNG use
//! would be caught by the determinism gate.

/// SplitMix64 generator. Small, fast, deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    /// Raw internal state, included in the world state hash.
    pub fn state(&self) -> u64 {
        self.state
    }

    /// Next pseudo-random u64 (SplitMix64).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f32 in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        // Use the top 24 bits for the f32 mantissa precision.
        let bits = (self.next_u64() >> 40) as u32; // 24 bits
        (bits as f32) / ((1u32 << 24) as f32)
    }

    /// Uniform f32 in `[min, max)`.
    pub fn range_f32(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.next_f32()
    }
}

//! Operating-system randomness without external crates.
//!
//! Network-facing values (heartbeat nonces, session ids) must be
//! unpredictable, so they come from the kernel CSPRNG via `/dev/urandom`
//! rather than from a seeded PRNG. On platforms without `/dev/urandom` this
//! returns an error instead of silently degrading to a predictable source.

use std::fs::File;
use std::io::{self, Read};

/// Fill `buf` with bytes from the OS CSPRNG.
pub fn fill_random(buf: &mut [u8]) -> io::Result<()> {
    let mut file = File::open("/dev/urandom").map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("OS randomness unavailable (/dev/urandom): {e}"),
        )
    })?;
    file.read_exact(buf)
}

/// `n` random bytes from the OS CSPRNG.
pub fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    fill_random(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_distinct_values() {
        let a = random_bytes(16).unwrap();
        let b = random_bytes(16).unwrap();
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
    }
}

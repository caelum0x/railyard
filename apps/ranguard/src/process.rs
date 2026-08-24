//! Process attestation — OWN SANDBOX ONLY.
//!
//! The tool launches its OWN "game" child process (a subcommand of this very
//! binary) and periodically re-verifies it: the child must still be the exact
//! process we spawned (we keep its `Child` handle — we never look up arbitrary
//! PIDs), and the on-disk binary must still hash to the value we trusted at
//! launch time.
//!
//! HARD SCOPE LIMIT: we only ever inspect a process WE started and a binary WE
//! own. There is no reading of other processes' memory, no attaching to foreign
//! PIDs, and no anti-debugging. That would be out of scope for a defensive,
//! userspace, opt-in tool.

use crate::manifest::hash_file;
use crate::sha256::to_hex;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Result of a single attestation check against our spawned child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestOutcome {
    /// Child is alive and its backing binary still matches the trusted hash.
    Ok { pid: u32 },
    /// The child process has exited (or was killed/replaced by the OS).
    Exited,
    /// The on-disk binary hash no longer matches what we recorded at launch.
    BinaryTampered { expected: String, actual: String },
}

/// A handle to the tool's own spawned "game" process plus its trusted identity.
pub struct GameProcess {
    child: Child,
    pid: u32,
    binary_path: PathBuf,
    expected_hash_hex: String,
}

impl GameProcess {
    /// Launch `binary` with `args` as our child, recording its trusted identity
    /// (PID + binary hash) at this "trusted" launch moment.
    pub fn launch(binary: &Path, args: &[&str]) -> io::Result<GameProcess> {
        let expected = hash_file(binary)?;
        let child = Command::new(binary).args(args).spawn()?;
        let pid = child.id();
        Ok(GameProcess {
            child,
            pid,
            binary_path: binary.to_path_buf(),
            expected_hash_hex: to_hex(&expected),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn expected_hash_hex(&self) -> &str {
        &self.expected_hash_hex
    }

    /// Attest the child we spawned: is it still alive, and does its binary still
    /// match the trusted hash? Uses only our own `Child` handle.
    pub fn attest(&mut self) -> io::Result<AttestOutcome> {
        // Re-hash the backing binary and compare to the trusted value.
        let actual = to_hex(&hash_file(&self.binary_path)?);
        if actual != self.expected_hash_hex {
            return Ok(AttestOutcome::BinaryTampered {
                expected: self.expected_hash_hex.clone(),
                actual,
            });
        }
        // Liveness of OUR child only. `try_wait` is non-blocking.
        match self.child.try_wait()? {
            Some(_status) => Ok(AttestOutcome::Exited),
            None => Ok(AttestOutcome::Ok { pid: self.pid }),
        }
    }

    /// Stop the child we started (cleanup). Ignores "already gone" errors.
    pub fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Uses a portable, always-present child so the test does not depend on the
    // demo binary being built. On unix `sleep` exists; this stays in-sandbox by
    // only ever touching the Child handle we own.
    #[cfg(unix)]
    #[test]
    fn attests_own_child_then_sees_exit() {
        let sleep = Path::new("/bin/sleep");
        if !sleep.exists() {
            return; // environment without /bin/sleep; skip gracefully
        }
        let mut proc = GameProcess::launch(sleep, &["0.3"]).unwrap();
        // Immediately after launch it should be alive with a matching binary.
        match proc.attest().unwrap() {
            AttestOutcome::Ok { pid } => assert_eq!(pid, proc.pid()),
            other => panic!("expected Ok, got {other:?}"),
        }
        // Wait for it to finish, then attestation should report Exited.
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(proc.attest().unwrap(), AttestOutcome::Exited);
    }
}

//! Heartbeat challenge-response with a rolling HMAC chain.
//!
//! A "server" issues a fresh challenge (monotonic counter + random nonce). A
//! "client" answers with HMAC(key, counter || nonce || prev_tag). Because each
//! tag folds in the previous accepted tag, and the server only accepts a
//! strictly increasing counter, a naive replay of a captured heartbeat is
//! rejected, and a forged heartbeat (wrong shared key) fails the MAC check.
//!
//! Both sides advance their rolling chain ONLY on acceptance: the server does so
//! inside `verify`, and the client does so via `commit` after it learns the
//! heartbeat was accepted (mirroring an acknowledgement). This keeps the two
//! chains in lockstep — a dropped or rejected heartbeat advances neither side, so
//! an honest client never permanently desyncs.
//!
//! Nonces: [`Server::new_secure`] draws every nonce from the OS CSPRNG
//! (`/dev/urandom`), which is what the network service (`ranguard-server`)
//! uses. [`Server::new`] keeps a seeded SplitMix64 source for deterministic
//! tests only — its nonces are predictable and must not face a network.
//!
//! SCOPE: anti-replay and key possession only. It provides no confidentiality;
//! run the HTTP service behind TLS.

use std::io;

use crate::hmac::{constant_time_eq, hmac_sha256};

/// A challenge issued by the server.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub counter: u64,
    pub nonce: [u8; 16],
}

/// A client's response to a challenge.
#[derive(Debug, Clone)]
pub struct Heartbeat {
    pub counter: u64,
    pub nonce: [u8; 16],
    pub tag: [u8; 32],
}

/// Why a heartbeat was accepted or rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    RejectedStaleCounter,
    RejectedBadNonce,
    RejectedBadMac,
}

/// Deterministic, dependency-free PRNG (SplitMix64) for nonce generation.
/// Not cryptographically strong — fine for a local demo nonce source.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn fill(&mut self, out: &mut [u8; 16]) {
        out[..8].copy_from_slice(&self.next_u64().to_le_bytes());
        out[8..].copy_from_slice(&self.next_u64().to_le_bytes());
    }
}

/// Bytes the tag is computed over: counter, nonce, and the rolling chain value.
fn message(counter: u64, nonce: &[u8; 16], prev_tag: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(8 + 16 + 32);
    m.extend_from_slice(&counter.to_be_bytes());
    m.extend_from_slice(nonce);
    m.extend_from_slice(prev_tag);
    m
}

/// Where challenge nonces come from.
enum NonceSource {
    /// Deterministic PRNG (tests / offline demos only).
    Seeded(SplitMix64),
    /// OS CSPRNG — required for anything network-facing.
    Os,
}

/// The verifying side ("server").
pub struct Server {
    key: Vec<u8>,
    counter: u64,
    last_accepted: u64,
    rolling_prev: [u8; 32],
    pending: Option<Challenge>,
    nonces: NonceSource,
}

impl Server {
    /// A server with a deterministic, SEEDED nonce source. Predictable: use
    /// only for tests and offline demos. Network services use
    /// [`Server::new_secure`].
    pub fn new(key: &[u8], seed: u64) -> Self {
        Self::with_source(key, NonceSource::Seeded(SplitMix64::new(seed)))
    }

    /// A server whose nonces come from the OS CSPRNG. Fails if OS randomness
    /// is unavailable rather than falling back to a predictable source.
    pub fn new_secure(key: &[u8]) -> io::Result<Self> {
        // Probe once so a missing /dev/urandom fails at construction.
        crate::rng::fill_random(&mut [0u8; 1])?;
        Ok(Self::with_source(key, NonceSource::Os))
    }

    fn with_source(key: &[u8], nonces: NonceSource) -> Self {
        Server {
            key: key.to_vec(),
            counter: 0,
            last_accepted: 0,
            rolling_prev: [0u8; 32],
            pending: None,
            nonces,
        }
    }

    /// Issue the next challenge (monotonic counter + fresh nonce).
    ///
    /// Panics only if the OS CSPRNG fails mid-run on a [`Server::new_secure`]
    /// server; services should call [`Server::try_issue_challenge`].
    pub fn issue_challenge(&mut self) -> Challenge {
        self.try_issue_challenge()
            .expect("OS randomness failed while issuing a heartbeat challenge")
    }

    /// Issue the next challenge, surfacing an OS randomness failure.
    pub fn try_issue_challenge(&mut self) -> io::Result<Challenge> {
        let mut nonce = [0u8; 16];
        match &mut self.nonces {
            NonceSource::Seeded(rng) => rng.fill(&mut nonce),
            NonceSource::Os => crate::rng::fill_random(&mut nonce)?,
        }
        self.counter += 1;
        let challenge = Challenge {
            counter: self.counter,
            nonce,
        };
        self.pending = Some(challenge.clone());
        Ok(challenge)
    }

    /// Counter of the most recently issued challenge.
    pub fn issued(&self) -> u64 {
        self.counter
    }

    /// Counter of the most recently accepted heartbeat (0 before the first).
    pub fn last_accepted(&self) -> u64 {
        self.last_accepted
    }

    /// The outstanding (issued, not yet accepted) challenge, if any.
    pub fn pending(&self) -> Option<&Challenge> {
        self.pending.as_ref()
    }

    /// Verify a heartbeat against the last issued challenge and the chain state.
    /// On acceptance the server advances its rolling chain and counter floor.
    pub fn verify(&mut self, hb: &Heartbeat) -> Outcome {
        // Anti-replay: counter must strictly exceed the last accepted one.
        if hb.counter <= self.last_accepted {
            return Outcome::RejectedStaleCounter;
        }
        // The nonce must match the challenge we actually issued.
        match &self.pending {
            Some(c) if c.counter == hb.counter && c.nonce == hb.nonce => {}
            _ => return Outcome::RejectedBadNonce,
        }
        // Recompute the expected tag over the current chain value.
        let expected = hmac_sha256(
            &self.key,
            &message(hb.counter, &hb.nonce, &self.rolling_prev),
        );
        if !constant_time_eq(&expected, &hb.tag) {
            return Outcome::RejectedBadMac;
        }
        // Accept: advance chain + counter floor, consume the challenge.
        self.rolling_prev = hb.tag;
        self.last_accepted = hb.counter;
        self.pending = None;
        Outcome::Accepted
    }
}

/// The responding side ("client"). Holds the shared key and its own chain copy.
pub struct Client {
    key: Vec<u8>,
    rolling_prev: [u8; 32],
}

impl Client {
    pub fn new(key: &[u8]) -> Self {
        Client {
            key: key.to_vec(),
            rolling_prev: [0u8; 32],
        }
    }

    /// Compute a response to a challenge over the client's current chain value.
    ///
    /// This is pure: it does NOT advance the client's rolling chain. The chain
    /// only moves forward once the server confirms acceptance (see `commit`),
    /// which is what keeps client and server chains in lockstep even if this
    /// heartbeat is dropped in transit or rejected.
    pub fn respond(&self, challenge: &Challenge) -> Heartbeat {
        let tag = hmac_sha256(
            &self.key,
            &message(challenge.counter, &challenge.nonce, &self.rolling_prev),
        );
        Heartbeat {
            counter: challenge.counter,
            nonce: challenge.nonce,
            tag,
        }
    }

    /// Advance the client's rolling chain after the server accepted `hb`.
    ///
    /// Mirrors receiving an acknowledgement. Call this ONLY when the server
    /// returned `Outcome::Accepted` for `hb`; the server advances its own chain
    /// to the same value inside `verify`, so the two stay synchronized.
    pub fn commit(&mut self, hb: &Heartbeat) {
        self.rolling_prev = hb.tag;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_server_accepts_honest_client_with_unpredictable_nonces() {
        let key = b"shared-secret";
        let mut server = Server::new_secure(key).unwrap();
        let mut client = Client::new(key);
        let a = server.try_issue_challenge().unwrap();
        let hb = client.respond(&a);
        assert_eq!(server.verify(&hb), Outcome::Accepted);
        client.commit(&hb);
        let b = server.try_issue_challenge().unwrap();
        assert_ne!(a.nonce, b.nonce);
    }

    #[test]
    fn honest_client_is_accepted() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 42);
        let mut client = Client::new(key);
        for _ in 0..5 {
            let ch = server.issue_challenge();
            let hb = client.respond(&ch);
            assert_eq!(server.verify(&hb), Outcome::Accepted);
            client.commit(&hb);
        }
    }

    #[test]
    fn replayed_heartbeat_is_rejected() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 7);
        let mut client = Client::new(key);

        let ch1 = server.issue_challenge();
        let hb1 = client.respond(&ch1);
        assert_eq!(server.verify(&hb1), Outcome::Accepted);
        client.commit(&hb1);

        // Advance the conversation legitimately.
        let ch2 = server.issue_challenge();
        let hb2 = client.respond(&ch2);
        assert_eq!(server.verify(&hb2), Outcome::Accepted);
        client.commit(&hb2);

        // Attacker re-sends the old, already-accepted heartbeat.
        assert_eq!(server.verify(&hb1), Outcome::RejectedStaleCounter);
    }

    #[test]
    fn forged_heartbeat_wrong_key_is_rejected() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 99);
        let attacker = Client::new(b"WRONG-key");

        let ch = server.issue_challenge();
        let forged = attacker.respond(&ch); // correct counter+nonce, bad tag
        assert_eq!(server.verify(&forged), Outcome::RejectedBadMac);
    }

    #[test]
    fn tampered_nonce_is_rejected() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 1);
        let client = Client::new(key);
        let ch = server.issue_challenge();
        let mut hb = client.respond(&ch);
        hb.nonce[0] ^= 0xFF; // flip the nonce
        assert_eq!(server.verify(&hb), Outcome::RejectedBadNonce);
    }

    // Invariant: because both sides advance the rolling chain ONLY on acceptance,
    // a dropped or rejected heartbeat must not permanently desync an honest
    // client. The client keeps its chain until the server acknowledges.
    #[test]
    fn dropped_or_rejected_heartbeat_does_not_desync_honest_client() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 2024);
        let mut client = Client::new(key);

        // Round 1: accepted normally.
        let ch1 = server.issue_challenge();
        let hb1 = client.respond(&ch1);
        assert_eq!(server.verify(&hb1), Outcome::Accepted);
        client.commit(&hb1);

        // Round 2: heartbeat is "dropped" — the server never sees it, so the
        // client correctly does NOT commit its chain.
        let ch2 = server.issue_challenge();
        let _dropped = client.respond(&ch2);

        // Round 3: a tampered heartbeat is rejected; again the client does not
        // commit, so its chain is unchanged.
        let ch3 = server.issue_challenge();
        let mut bad = client.respond(&ch3);
        bad.nonce[0] ^= 0xFF;
        assert_eq!(server.verify(&bad), Outcome::RejectedBadNonce);

        // Round 4: the honest client must still be accepted despite the earlier
        // drop and rejection — no permanent desync.
        let ch4 = server.issue_challenge();
        let hb4 = client.respond(&ch4);
        assert_eq!(server.verify(&hb4), Outcome::Accepted);
        client.commit(&hb4);
    }
}

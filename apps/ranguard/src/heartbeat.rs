//! Heartbeat challenge-response with a rolling HMAC chain.
//!
//! A "server" issues a fresh challenge (monotonic counter + random nonce). A
//! "client" answers with HMAC(key, counter || nonce || prev_tag). Because each
//! tag folds in the previous accepted tag, and the server only accepts a
//! strictly increasing counter, a naive replay of a captured heartbeat is
//! rejected, and a forged heartbeat (wrong shared key) fails the MAC check.
//!
//! SCOPE: this is a local shared-secret demo of anti-replay. It is NOT a
//! network security protocol and provides no confidentiality.

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

/// The verifying side ("server").
pub struct Server {
    key: Vec<u8>,
    counter: u64,
    last_accepted: u64,
    rolling_prev: [u8; 32],
    pending: Option<Challenge>,
    rng: SplitMix64,
}

impl Server {
    pub fn new(key: &[u8], seed: u64) -> Self {
        Server {
            key: key.to_vec(),
            counter: 0,
            last_accepted: 0,
            rolling_prev: [0u8; 32],
            pending: None,
            rng: SplitMix64::new(seed),
        }
    }

    /// Issue the next challenge (monotonic counter + fresh nonce).
    pub fn issue_challenge(&mut self) -> Challenge {
        self.counter += 1;
        let mut nonce = [0u8; 16];
        self.rng.fill(&mut nonce);
        let challenge = Challenge {
            counter: self.counter,
            nonce,
        };
        self.pending = Some(challenge.clone());
        challenge
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
        let expected = hmac_sha256(&self.key, &message(hb.counter, &hb.nonce, &self.rolling_prev));
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

    /// Answer a challenge, advancing the client's local chain to match server.
    pub fn respond(&mut self, challenge: &Challenge) -> Heartbeat {
        let tag = hmac_sha256(
            &self.key,
            &message(challenge.counter, &challenge.nonce, &self.rolling_prev),
        );
        self.rolling_prev = tag;
        Heartbeat {
            counter: challenge.counter,
            nonce: challenge.nonce,
            tag,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honest_client_is_accepted() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 42);
        let mut client = Client::new(key);
        for _ in 0..5 {
            let ch = server.issue_challenge();
            let hb = client.respond(&ch);
            assert_eq!(server.verify(&hb), Outcome::Accepted);
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

        // Advance the conversation legitimately.
        let ch2 = server.issue_challenge();
        let _hb2 = client.respond(&ch2);

        // Attacker re-sends the old, already-accepted heartbeat.
        assert_eq!(server.verify(&hb1), Outcome::RejectedStaleCounter);
    }

    #[test]
    fn forged_heartbeat_wrong_key_is_rejected() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 99);
        let mut attacker = Client::new(b"WRONG-key");

        let ch = server.issue_challenge();
        let forged = attacker.respond(&ch); // correct counter+nonce, bad tag
        assert_eq!(server.verify(&forged), Outcome::RejectedBadMac);
    }

    #[test]
    fn tampered_nonce_is_rejected() {
        let key = b"shared-secret";
        let mut server = Server::new(key, 1);
        let mut client = Client::new(key);
        let ch = server.issue_challenge();
        let mut hb = client.respond(&ch);
        hb.nonce[0] ^= 0xFF; // flip the nonce
        assert_eq!(server.verify(&hb), Outcome::RejectedBadNonce);
    }
}

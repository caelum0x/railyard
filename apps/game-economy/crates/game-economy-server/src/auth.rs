//! Inbound auth for the storefront.
//!
//! The audit found the money path was wide open: `POST /v1/purchase` accepted an
//! arbitrary `player` field and charged THAT Mint account with no authentication,
//! so anyone could charge (grief) anyone. This module closes that by binding every
//! money-path request to the identity it can prove.
//!
//! ## Scheme — a BLAKE3-keyed bearer token
//! The storefront shares one secret (`GAME_AUTH_SECRET`) with the game backend
//! that fronts it. A player's token is
//!
//! ```text
//!   <player>.<hex( BLAKE3-keyed-hash(key, player) )>
//! ```
//!
//! where `key = BLAKE3(secret)`. The backend (which holds the secret) issues these
//! to its authenticated players; the storefront only *verifies* them and then
//! rejects any request whose `player` is not the token's proven principal. This
//! matches the repo's inbound-auth convention (mint-server / abacus-proxy both
//! gate ingress on a shared secret compared in CONSTANT TIME via `subtle`) and
//! extends it with per-principal binding — which a bare shared key cannot give.
//! We reuse the already-present `blake3` dependency instead of pulling `subtle`:
//! `blake3::keyed_hash` is a real MAC and `blake3::Hash`'s `PartialEq` is
//! constant-time, so tag comparison has no timing side channel and no new crate is
//! introduced.
//!
//! Fail-closed: a missing, malformed, or forged token is always `401`; the guard
//! has no bypass.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use game_economy_core::{Fulfiller, MintTransport};

use crate::{unauthorized, AppState};

/// The authenticated principal — a Mint player account id proven by a valid
/// bearer token. [`require_auth`] injects it into request extensions; the
/// money-path handlers read it to reject acting for another player.
#[derive(Clone, Debug)]
pub struct Principal(pub String);

/// Derive the 32-byte signing key from the shared secret. Hashing lets the
/// operator use any-length `GAME_AUTH_SECRET` while `keyed_hash` needs exactly 32
/// bytes.
pub fn derive_key(secret: &[u8]) -> [u8; 32] {
    *blake3::hash(secret).as_bytes()
}

/// Mint the bearer token that authenticates `player`. Issued by the secret-holding
/// game backend; the storefront only verifies. Exposed so operator tooling and the
/// server's own tests generate tokens exactly the way production does.
pub fn player_token(key: &[u8; 32], player: &str) -> String {
    let tag = blake3::keyed_hash(key, player.as_bytes());
    format!("{player}.{}", tag.to_hex())
}

/// Verify `token` against `key`, returning the authenticated player on a
/// constant-time tag match. `None` on any malformation or mismatch (fail closed).
pub fn verify_token(key: &[u8; 32], token: &str) -> Option<String> {
    let (player, tag_hex) = token.rsplit_once('.')?;
    if player.is_empty() {
        return None;
    }
    // Parse first so a garbled tag can't panic; then compare the fixed-size hashes
    // in constant time (`blake3::Hash: PartialEq` is documented constant-time).
    let provided = blake3::Hash::from_hex(tag_hex).ok()?;
    let expected = blake3::keyed_hash(key, player.as_bytes());
    if expected == provided {
        Some(player.to_string())
    } else {
        None
    }
}

/// Middleware guarding the money-path routes. Requires `Authorization: Bearer
/// <token>` and stashes the proven [`Principal`] for the handler; any
/// missing/garbled/forged token is `401`.
pub async fn require_auth<M, F>(
    State(state): State<AppState<M, F>>,
    mut req: Request,
    next: Next,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let principal = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .map(str::trim)
        .and_then(|token| verify_token(state.auth_key(), token));

    match principal {
        Some(player) => {
            req.extensions_mut().insert(Principal(player));
            next.run(req).await
        }
        None => unauthorized(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_valid_token() {
        let key = derive_key(b"top-secret");
        let token = player_token(&key, "acct_alice");
        assert_eq!(verify_token(&key, &token).as_deref(), Some("acct_alice"));
    }

    #[test]
    fn rejects_a_forged_or_tampered_tag() {
        let key = derive_key(b"top-secret");
        // A tag minted for a DIFFERENT player must not authenticate acct_alice.
        let bob = player_token(&key, "acct_bob");
        let forged = format!("acct_alice.{}", bob.rsplit_once('.').unwrap().1);
        assert_eq!(verify_token(&key, &forged), None);
    }

    #[test]
    fn rejects_wrong_key() {
        let token = player_token(&derive_key(b"secret-a"), "acct_alice");
        assert_eq!(verify_token(&derive_key(b"secret-b"), &token), None);
    }

    #[test]
    fn rejects_malformed_tokens() {
        let key = derive_key(b"top-secret");
        assert_eq!(verify_token(&key, "no-dot-here"), None);
        assert_eq!(verify_token(&key, ".deadbeef"), None);
        assert_eq!(verify_token(&key, "acct_alice.not-hex"), None);
        assert_eq!(verify_token(&key, "acct_alice."), None);
    }

    #[test]
    fn player_with_dots_survives_rsplit() {
        // Mint account ids can contain dots; rsplit on the last `.` keeps them.
        let key = derive_key(b"top-secret");
        let token = player_token(&key, "acct.alice.v2");
        assert_eq!(verify_token(&key, &token).as_deref(), Some("acct.alice.v2"));
    }
}

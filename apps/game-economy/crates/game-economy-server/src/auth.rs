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

/// Mint an EXPIRING bearer token for `player`, valid until `expires_at` (unix
/// seconds). Format: `v2.<expires_at>.<player>.<hex tag>` where the tag is a
/// BLAKE3-keyed MAC over a domain-separated `(expires_at, player)` preimage, so
/// neither field can be altered. This is what `POST /v1/tokens` issues.
pub fn player_token_expiring(key: &[u8; 32], player: &str, expires_at: u64) -> String {
    let tag = blake3::keyed_hash(key, v2_preimage(player, expires_at).as_bytes());
    format!("v2.{expires_at}.{player}.{}", tag.to_hex())
}

fn v2_preimage(player: &str, expires_at: u64) -> String {
    format!("game-economy-token:v2\0{expires_at}\0{player}")
}

/// Current unix time in seconds (0 if the clock is before the epoch).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verify `token` against `key`, returning the authenticated player on a
/// constant-time tag match. Accepts both expiring (`v2.`) and legacy
/// non-expiring tokens. `None` on any malformation, mismatch or expiry.
pub fn verify_token(key: &[u8; 32], token: &str) -> Option<String> {
    verify_token_at(key, token, now_unix(), true)
}

/// Full verifier: `now` is the unix time used for expiry, `allow_legacy`
/// controls whether non-expiring legacy tokens are still honoured.
pub fn verify_token_at(
    key: &[u8; 32],
    token: &str,
    now: u64,
    allow_legacy: bool,
) -> Option<String> {
    if let Some(player) = verify_v2(key, token, now) {
        return Some(player);
    }
    if allow_legacy {
        verify_legacy(key, token)
    } else {
        None
    }
}

fn verify_v2(key: &[u8; 32], token: &str, now: u64) -> Option<String> {
    let rest = token.strip_prefix("v2.")?;
    let (exp_str, rest) = rest.split_once('.')?;
    let (player, tag_hex) = rest.rsplit_once('.')?;
    if player.is_empty() {
        return None;
    }
    let expires_at: u64 = exp_str.parse().ok()?;
    let provided = blake3::Hash::from_hex(tag_hex).ok()?;
    let expected = blake3::keyed_hash(key, v2_preimage(player, expires_at).as_bytes());
    if expected != provided || now >= expires_at {
        return None;
    }
    Some(player.to_string())
}

fn verify_legacy(key: &[u8; 32], token: &str) -> Option<String> {
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

/// Hash a game-server credential for storage/comparison. Stored hashed so the
/// constant-time `blake3::Hash` equality can compare it against a presented key.
pub fn server_credential(secret: &[u8]) -> blake3::Hash {
    blake3::derive_key("railyard game-economy server credential v1", secret).into()
}

/// The 32-byte key pack-opening seeds are derived under. Uses BLAKE3's
/// derive-key mode (a separate domain from the keyed-hash token MAC), so no
/// player token can ever equal or reveal it.
pub fn pack_seed_key(auth_key: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("railyard game-economy pack seed v1", auth_key)
}

/// The server-committed seed for opening `entitlement_id`. Players cannot
/// compute it (it is keyed by the server secret) and cannot influence it (it is
/// a pure function of the pack's identity), so drops cannot be ground offline.
pub fn pack_seed(seed_key: &[u8; 32], player: &str, entitlement_id: &str) -> u64 {
    let preimage = format!("packseed:v1\0{player}\0{entitlement_id}");
    let hash = blake3::keyed_hash(seed_key, preimage.as_bytes());
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(buf)
}

/// Middleware guarding the money-path routes. Requires `Authorization: Bearer
/// <token>` and stashes the proven [`Principal`] for the handler; any
/// missing/garbled/forged/expired token is `401`.
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
        .and_then(|token| {
            verify_token_at(
                state.auth_key(),
                token,
                now_unix(),
                state.allow_legacy_tokens(),
            )
        });

    match principal {
        Some(player) => {
            req.extensions_mut().insert(Principal(player));
            next.run(req).await
        }
        None => unauthorized(),
    }
}

/// Header carrying the game-backend credential on server-to-server routes.
pub const SERVER_KEY_HEADER: &str = "x-game-server-key";

/// Middleware guarding server-to-server routes (battle-pass progression, token
/// issuance, entitlement revocation). Only the game backend holding
/// `GAME_SERVER_SECRET` may call them; a PLAYER bearer token is never enough.
/// Fails closed with `503` when no server credential is configured.
pub async fn require_server<M, F>(
    State(state): State<AppState<M, F>>,
    req: Request,
    next: Next,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let Some(expected) = state.server_key() else {
        return crate::error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "server_auth_not_configured",
            "GAME_SERVER_SECRET is not configured; server-to-server routes are disabled",
        );
    };
    let presented = req
        .headers()
        .get(SERVER_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| server_credential(v.as_bytes()));
    match presented {
        Some(hash) if hash == *expected => next.run(req).await,
        _ => crate::error(
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid game server credential",
        ),
    }
}

/// Middleware guarding the operator console API (`/v1/admin/*`). Requires
/// `Authorization: Bearer <GAME_ADMIN_TOKEN>`; fails closed with `503` when no
/// admin token is configured. Player tokens and the game-server key are never
/// accepted here.
pub async fn require_admin<M, F>(
    State(state): State<AppState<M, F>>,
    req: Request,
    next: Next,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let Some(expected) = state.admin_key() else {
        return crate::error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "admin_not_configured",
            "GAME_ADMIN_TOKEN is not configured; the operator console is disabled",
        );
    };
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| admin_credential(v.as_bytes()));
    match presented {
        Some(hash) if hash == *expected => next.run(req).await,
        _ => crate::error(
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid admin token",
        ),
    }
}

/// Hash of the operator admin token (domain-separated from the server key so
/// the two secrets can never be confused).
pub fn admin_credential(secret: &[u8]) -> blake3::Hash {
    blake3::derive_key("game-economy admin token v1", secret).into()
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
    fn expiring_tokens_verify_until_expiry_and_reject_tampering() {
        let key = derive_key(b"top-secret");
        let token = player_token_expiring(&key, "acct.alice", 1_000);
        assert_eq!(
            verify_token_at(&key, &token, 999, false).as_deref(),
            Some("acct.alice")
        );
        assert_eq!(verify_token_at(&key, &token, 1_000, false), None);
        let bumped = token.replacen("v2.1000.", "v2.9999.", 1);
        assert_eq!(verify_token_at(&key, &bumped, 999, false), None);
        // Legacy tokens are refused when legacy support is off.
        let legacy = player_token(&key, "acct_alice");
        assert_eq!(verify_token_at(&key, &legacy, 0, false), None);
    }

    #[test]
    fn player_with_dots_survives_rsplit() {
        // Mint account ids can contain dots; rsplit on the last `.` keeps them.
        let key = derive_key(b"top-secret");
        let token = player_token(&key, "acct.alice.v2");
        assert_eq!(verify_token(&key, &token).as_deref(), Some("acct.alice.v2"));
    }
}

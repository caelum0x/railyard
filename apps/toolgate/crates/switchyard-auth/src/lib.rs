//! # switchyard-auth
//!
//! - [`relay_key`]: one key format for both rails, Relay's
//!   `sk-t0-<public_id>-<long_key>`, validated against Relay's own key table
//!   (`tensorzero_auth_api_key`: SHA-256 of the long key), or an in-memory
//!   store for tests and dev.
//! - [`scopes`]: `tools:read`, `tools:call`, `tools:write`, `tools:send`,
//!   `tools:spend`, `admin`.
//! - [`jwt`]: 10-minute EdDSA (Ed25519) access tokens bound to one resource.
//! - [`oauth`]: the OAuth 2.1 authorization server logic for MCP clients and
//!   the CLI: PKCE S256, dynamic client registration, client ID metadata
//!   documents, rotating hashed refresh tokens with family revocation on reuse,
//!   and the device authorization grant.
//! - [`identity`]: who the human at the consent screen is (Scribase sign-in).

pub mod identity;
pub mod jwt;
pub mod oauth;
pub mod relay_key;
pub mod scopes;

pub use identity::{IdentityProvider, ScribaseIdentity, StaticIdentity, User};
pub use jwt::{AccessClaims, TokenSigner};
pub use relay_key::{KeyVerifier, MemRelayKeys, PgRelayKeys, VerifiedKey};

use rand::RngCore;
use sha2::{Digest, Sha256};

/// 32 random bytes, base64url without padding.
pub fn random_token() -> String {
    use base64::Engine;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Hex SHA-256 used to store tokens and codes hashed.
pub fn hash_token(t: &str) -> String {
    hex::encode(Sha256::digest(t.as_bytes()))
}

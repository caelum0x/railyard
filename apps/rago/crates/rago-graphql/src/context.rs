//! GraphQL context + auth guard.
//!
//! The dashboard authenticates with a session/JWT rather than an API key. Here
//! every resolver is gated behind a real [`Principal`] that the HTTP layer
//! resolves from the request's bearer token against the database (the same
//! `api_keys`-backed lookup used by REST) and injects into the request context.
//! There is no bypass — without a resolved principal, resolvers return an
//! authentication error. Dashboard JWT/session issuance is a documented
//! follow-up; the guard itself is real.

use async_graphql::{Context, Result};
use rago_core::models::Organization;
use rago_db::Db;
use uuid::Uuid;

/// Schema-level data: the database handle.
pub struct GraphQlData {
    pub db: Db,
}

/// The authenticated principal for a GraphQL request.
#[derive(Clone)]
pub struct Principal {
    pub organization: Organization,
}

/// The dashboard user session, injected by the HTTP layer when the bearer token
/// is a session JWT (rather than an API key). Optional — API-key requests carry
/// only an organization principal.
#[derive(Clone)]
pub struct UserSession {
    pub id: Uuid,
    pub email: String,
}

/// The database handle (always present).
pub fn db<'a>(ctx: &'a Context<'_>) -> &'a Db {
    &ctx.data_unchecked::<GraphQlData>().db
}

/// The authenticated organization, or an authentication error.
pub fn require_org<'a>(ctx: &'a Context<'_>) -> Result<&'a Organization> {
    match ctx.data_opt::<Principal>() {
        Some(p) => Ok(&p.organization),
        None => Err(async_graphql::Error::new("unauthorized")),
    }
}

/// The dashboard user session for the request, if a JWT was presented.
pub fn current_user<'a>(ctx: &'a Context<'_>) -> Option<&'a UserSession> {
    ctx.data_opt::<UserSession>()
}

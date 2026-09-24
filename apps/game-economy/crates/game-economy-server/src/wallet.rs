//! Player wallet reads and player-account provisioning.
//!
//! - `GET /v1/wallet/:player` (player bearer token) — the player's spendable
//!   Mint balance plus the entitlements the store has granted them, in one read,
//!   so a game's shop screen needs a single call. A player may only read their
//!   own wallet.
//! - `POST /v1/players` (game-server key) — open a Mint account for a new player.
//!   Mint account ids are `acct_<kind><ULID>`, so a game cannot invent them; its
//!   backend calls this once per player and stores the returned `account_id`
//!   (e.g. a unique column next to its own player id) and uses it as `player`
//!   everywhere else in this API.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use game_economy_core::{Fulfiller, MintTransport};

use crate::auth::Principal;
use crate::{error, forbidden, game_error, ok, AppState};

/// Longest `owner_ref` accepted (Mint's own limit).
const MAX_OWNER_REF: usize = 256;

/// `GET /v1/wallet/:player` — balance + inventory for the authenticated player.
pub(crate) async fn wallet_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Extension(principal): Extension<Principal>,
    Path(player): Path<String>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    if player != principal.0 {
        return forbidden();
    }
    let balance = match state.store().mint().read_balance(&player).await {
        Ok(balance) => balance,
        Err(err) => return game_error(&err),
    };
    let inventory = match state.store().try_inventory(&player) {
        Ok(inventory) => serde_json::to_value(inventory).unwrap_or_else(|_| json!({})),
        Err(err) => return game_error(&err),
    };
    ok(json!({
        "player": player,
        "available_micros": balance.available_micros,
        "held_micros": balance.held_micros,
        "inventory": inventory,
    }))
}

#[derive(Deserialize)]
pub(crate) struct CreatePlayerReq {
    /// The game's own player id (recorded on the Mint account for audit).
    owner_ref: String,
}

/// `POST /v1/players` — SERVER-TO-SERVER: open a Mint user account for a player.
/// Not idempotent by design (Mint mints a fresh id per call): the caller stores
/// the returned id and reuses it; a lost response at worst leaves an empty,
/// unused account.
pub(crate) async fn create_player_handler<M, F>(
    State(state): State<AppState<M, F>>,
    Json(req): Json<CreatePlayerReq>,
) -> Response
where
    M: MintTransport + 'static,
    F: Fulfiller + 'static,
{
    let owner_ref = req.owner_ref.trim();
    if owner_ref.is_empty() || owner_ref.len() > MAX_OWNER_REF {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "owner_ref must be 1-256 characters",
        );
    }
    match state.store().mint().create_player_account(owner_ref).await {
        Ok(account_id) => ok(json!({ "account_id": account_id, "owner_ref": owner_ref })),
        Err(err) => game_error(&err),
    }
}

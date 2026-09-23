//! `GET /store` — the player-facing storefront page (static HTML + vanilla JS
//! over this server's own JSON API). The player signs in with the bearer token
//! their game backend issued via `POST /v1/tokens`; the page then lists the
//! catalog, buys SKUs (Mint hold -> capture), opens packs, shows the
//! inventory, and the signed receipt history.

use axum::response::Html;

pub(crate) async fn page() -> Html<&'static str> {
    Html(include_str!("../assets/store.html"))
}

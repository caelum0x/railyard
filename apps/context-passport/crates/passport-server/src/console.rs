//! `GET /console` — the Identity Lock web console (static HTML + vanilla JS
//! over this server's own `/v1` API, authenticated with the caller's
//! `X-API-Key`). Create, edit, lock and version characters/brands/personas,
//! generate AS a locked passport, and browse generations + C2PA manifests.

use axum::response::Html;

pub(crate) async fn page() -> Html<&'static str> {
    Html(include_str!("../assets/console.html"))
}

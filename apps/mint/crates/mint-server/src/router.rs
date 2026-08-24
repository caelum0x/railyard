//! Route table. `build_router` wires every §4 route to its handler, layers the
//! API-key auth middleware over the protected routes, and leaves `/healthz` +
//! `/readyz` unauthenticated.

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use mint_store::LedgerStore;
use tower_http::trace::TraceLayer;

use crate::auth;
use crate::handlers::{
    accounts, addresses, aitp, allowances, deposits, health, holds, mint_redeem, receipts, reserve,
    streams, transfers,
};
use crate::state::AppState;

/// Build the full application router for a concrete store `S`.
pub fn build_router<S: LedgerStore + 'static>(state: AppState<S>) -> Router {
    let protected = Router::new()
        // accounts + addresses
        .route("/v1/accounts", post(accounts::create::<S>))
        .route("/v1/accounts/:id", get(accounts::get::<S>))
        .route("/v1/accounts/:id/balance", get(accounts::balance::<S>))
        .route("/v1/accounts/:id/addresses", post(addresses::create::<S>))
        .route(
            "/v1/accounts/:id/receipts",
            get(receipts::account_receipts::<S>),
        )
        .route("/v1/addresses/:address", get(addresses::lookup::<S>))
        // transfers
        .route("/v1/transfers", post(transfers::create::<S>))
        // holds
        .route("/v1/holds", post(holds::create::<S>))
        .route("/v1/holds/:id", get(holds::get::<S>))
        .route("/v1/holds/:id/capture", post(holds::capture::<S>))
        .route("/v1/holds/:id/release", post(holds::release::<S>))
        // streams
        .route("/v1/streams", post(streams::open::<S>))
        .route("/v1/streams/:id", get(streams::get::<S>))
        .route("/v1/streams/:id/tick", post(streams::tick::<S>))
        .route("/v1/streams/:id/close", post(streams::close::<S>))
        // allowances
        .route("/v1/allowances", post(allowances::grant::<S>))
        .route("/v1/allowances/:id", get(allowances::get::<S>))
        .route("/v1/allowances/:id/spend", post(allowances::spend::<S>))
        .route("/v1/allowances/:id/revoke", post(allowances::revoke::<S>))
        // AITP-402 protocol
        .route("/v1/aitp/challenge", post(aitp::challenge::<S>))
        .route("/v1/aitp/verify", post(aitp::verify::<S>))
        .route("/v1/aitp/capture", post(aitp::capture::<S>))
        .route("/v1/aitp/release", post(aitp::release::<S>))
        // receipts + reserve + mint/redeem
        .route("/v1/receipts/audit", get(receipts::audit::<S>))
        .route("/v1/reserve", get(reserve::get::<S>))
        .route("/v1/mint", post(mint_redeem::mint::<S>))
        .route("/v1/redeem", post(mint_redeem::redeem::<S>))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key::<S>,
        ));

    // The on-ramp webhooks are authenticated SOLELY by their per-provider
    // signature (verified in `mint-payments` over the RAW body). They must NOT
    // sit behind the `x-api-key` middleware: providers cannot attach a custom
    // API-key header to their webhook deliveries, so gating on the API key would
    // reject every genuine delivery with 401 before signature verification ever
    // runs. One route per provider: `stripe`, `creem`, `polar`, `paypal`,
    // `revenuecat`, `adapty`, `usdc`, `iyzico` (dispatched by the `:provider` tag).
    let public = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz::<S>))
        .route(
            "/v1/deposits/:provider/webhook",
            post(deposits::provider_webhook::<S>),
        );

    protected
        .merge(public)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

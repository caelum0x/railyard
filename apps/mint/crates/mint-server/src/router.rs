//! Route table. `build_router` wires every §4 route to its handler, layers the
//! API-key auth middleware over the protected routes, and leaves `/healthz` +
//! `/readyz` unauthenticated.

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use mint_store::LedgerStore;
use tower_http::trace::TraceLayer;

use crate::auth;
use crate::handlers::{
    accounts, addresses, admin, aitp, allowances, checkouts, deposits, health, holds, keys, kyc,
    mint_redeem, payer_keys, payouts, positions, provider_reports, receipts, reserve, reversals,
    statements, streams, transfers, verification_keys,
};
use crate::state::AppState;

/// Build the full application router for a concrete store `S`.
pub fn build_router<S: LedgerStore + 'static>(state: AppState<S>) -> Router {
    let protected = Router::new()
        // accounts + addresses
        .route(
            "/v1/accounts",
            post(accounts::create::<S>).get(statements::list::<S>),
        )
        .route("/v1/accounts/:id", get(accounts::get::<S>))
        .route("/v1/accounts/:id/balance", get(accounts::balance::<S>))
        // account lifecycle (operator-only)
        .route("/v1/accounts/:id/freeze", post(accounts::freeze::<S>))
        .route("/v1/accounts/:id/unfreeze", post(accounts::unfreeze::<S>))
        .route("/v1/accounts/:id/close", post(accounts::close::<S>))
        .route("/v1/accounts/:id/addresses", post(addresses::create::<S>))
        .route(
            "/v1/accounts/:id/receipts",
            get(receipts::account_receipts::<S>),
        )
        .route(
            "/v1/accounts/:id/statement",
            get(statements::statement::<S>),
        )
        // open positions (what keeps `held` up / blocks a close)
        .route("/v1/accounts/:id/holds", get(positions::holds::<S>))
        .route("/v1/accounts/:id/streams", get(positions::streams::<S>))
        .route("/v1/accounts/:id/allowances", get(positions::allowances::<S>))
        .route("/v1/accounts/:id/kyc", get(kyc::status::<S>))
        .route("/v1/accounts/:id/kyc/session", post(kyc::start_session::<S>))
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
        // direct-to-Mint hosted funding checkout
        .route("/v1/checkouts", post(checkouts::create::<S>))
        // receipts + reserve + mint/redeem
        .route("/v1/receipts/audit", get(receipts::audit::<S>))
        .route("/v1/reserve", get(reserve::get::<S>))
        .route("/v1/mint", post(mint_redeem::mint::<S>))
        .route("/v1/redeem", post(mint_redeem::redeem::<S>))
        // payout tracking + reconciliation
        .route("/v1/payouts", get(payouts::list::<S>))
        .route("/v1/payouts/:payout_ref", get(payouts::get::<S>))
        .route("/v1/payouts/:payout_ref/retry", post(payouts::retry::<S>))
        .route("/v1/payouts/:payout_ref/cancel", post(payouts::cancel::<S>))
        .route("/v1/payouts/:payout_ref/mark-paid", post(payouts::mark_paid::<S>))
        .route("/v1/reconciliation", get(reserve::reconciliation::<S>))
        // provider settlement report imports + matching
        .route(
            "/v1/reconciliation/reports",
            post(provider_reports::import::<S>).get(provider_reports::list::<S>),
        )
        .route(
            "/v1/reconciliation/reports/:id",
            get(provider_reports::get::<S>).merge(delete(provider_reports::delete::<S>)),
        )
        // deposit reversals (refund / chargeback claw-backs)
        .route(
            "/v1/reversals",
            post(reversals::create::<S>).get(reversals::list::<S>),
        )
        .route("/v1/reversals/:deposit_ref/retry", post(reversals::retry::<S>))
        .route(
            "/v1/reversals/:deposit_ref/write-off",
            post(reversals::write_off::<S>),
        )
        // AITP-402 payer verifying keys (operator-only)
        .route(
            "/v1/payer-keys",
            get(payer_keys::list::<S>).post(payer_keys::create::<S>),
        )
        .route(
            "/v1/payer-keys/:payer/:pubkey_id/revoke",
            post(payer_keys::revoke::<S>),
        )
        // DB-backed API keys (operator-only)
        .route("/v1/keys", get(keys::list::<S>).post(keys::create::<S>))
        .route("/v1/keys/:id/revoke", post(keys::revoke::<S>))
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
            "/.well-known/mint-verification-keys.json",
            get(verification_keys::get::<S>),
        )
        .route(
            "/v1/deposits/:provider/webhook",
            post(deposits::provider_webhook::<S>),
        )
        // Buyer-facing checkout return surface (browsers carry no API key).
        .route("/checkout/success", get(checkouts::success_page))
        .route("/checkout/cancel", get(checkouts::cancel_page))
        .route(
            "/checkout/paypal/return",
            get(checkouts::paypal_return::<S>),
        )
        // Operator console shell (static; its data calls use X-API-Key).
        .route("/admin", get(admin::console));

    protected
        .merge(public)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

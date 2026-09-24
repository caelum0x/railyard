//! `GET /v1/providers` — which payment providers this deployment has
//! configured and what each can do, so clients (and the operator console)
//! offer only checkouts / payout rails that will actually work.

use axum::extract::State;
use mint_store::LedgerStore;
use serde_json::json;

use crate::error::{ok, ApiResult};
use crate::state::AppState;

/// `GET /v1/providers` → active providers with their capabilities, plus
/// whether Mint's public URL (needed by browser-return providers such as
/// PayPal and Iyzico) is configured.
pub async fn list<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let public_url = st.public.public_url.is_some();
    let items: Vec<serde_json::Value> = st
        .payments
        .as_ref()
        .map(|registry| {
            registry
                .capabilities()
                .into_iter()
                .map(|c| {
                    let tag = c.provider.tag();
                    // PayPal and Iyzico return the buyer's browser to Mint, so a
                    // checkout needs MINT_PUBLIC_URL as well as the keys.
                    let needs_public_url = matches!(tag, "paypal" | "iyzico");
                    let checkout = c.checkout && (!needs_public_url || public_url);
                    json!({
                        "provider": tag,
                        "checkout": checkout,
                        "payout": c.payout,
                        "webhook": c.webhook,
                        "checkout_blocked_reason": (c.checkout && !checkout)
                            .then_some("MINT_PUBLIC_URL is not set"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ok(json!({
        "configured": st.payments.is_some(),
        "public_url_configured": public_url,
        "items": items,
    })))
}

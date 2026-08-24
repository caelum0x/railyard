//! The gating middleware: run [`Turnstile::admit`], short-circuit `402`, and
//! settle the hold around the inner handler.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use turnstile_core::{Admit, Turnstile};

use crate::extractor::PaidContext;
use crate::response;

/// AITP-Authorization request header (lowercased for the case-insensitive map).
const HEADER_AUTHORIZATION: &str = "aitp-authorization";

/// Middleware for `axum::middleware::from_fn_with_state(gate, admit)`.
///
/// Flow:
/// 1. Derive the resource (`"<METHOD> <path>"`) and read the authorization header.
/// 2. [`Turnstile::admit`]:
///    - [`Admit::Challenge`] / [`Admit::Rejected`] → short-circuit `402`;
///    - [`Admit::Admitted`] → insert a [`PaidContext`], run the inner handler,
///      then capture the reported actual (default: full price) on a `2xx` or
///      release the hold otherwise.
pub async fn admit(State(gate): State<Arc<Turnstile>>, mut req: Request, next: Next) -> Response {
    let resource = format!("{} {}", req.method(), req.uri().path());
    let authorization = req
        .headers()
        .get(HEADER_AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    match gate.admit(authorization.as_deref(), &resource).await {
        Admit::Challenge(challenge) => response::challenge(&challenge),
        Admit::Rejected(error) => {
            let fresh = gate.challenge(&resource);
            response::rejected(&error, &fresh)
        }
        Admit::Admitted(settled) => {
            let reported = Arc::new(Mutex::new(None));
            req.extensions_mut()
                .insert(PaidContext::new(reported.clone(), gate.price()));

            let response = next.run(req).await;
            let succeeded = response.status().is_success();

            // Success-only: capture what the handler reported (or the full quoted
            // price if it reported nothing); release on any non-2xx.
            let actual = reported
                .lock()
                .map(|g| *g)
                .unwrap_or(None)
                .unwrap_or_else(|| gate.price());

            if succeeded {
                let receipt = settled.capture(actual).await.ok().flatten();
                response::attach_receipt(response, receipt)
            } else {
                let _ = settled.release().await;
                response
            }
        }
    }
}

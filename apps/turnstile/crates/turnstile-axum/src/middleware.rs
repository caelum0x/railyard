//! The gating middleware: run [`Turnstile::admit`], short-circuit `402`, and
//! settle the hold around the inner handler.

use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use turnstile_core::x402::{X402Admission, X402Gate, HEADER_PAYMENT};
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
pub async fn admit(State(gate): State<Arc<Turnstile>>, req: Request, next: Next) -> Response {
    gate_request(&gate, req, |req| next.run(req)).await
}

/// The gate as a plain async function, for servers that resolve the gate per
/// request (e.g. a runtime-editable tool registry) instead of binding one
/// fixed gate with `from_fn_with_state`. `inner` runs only for an admitted
/// request, with a [`PaidContext`] in its extensions.
///
/// The capture ceiling and default charge are the amount the payer actually
/// authorized (the price quoted when the challenge was issued), so a request
/// paid against a challenge issued before a reprice settles at its quoted price.
pub async fn gate_request<F, Fut>(gate: &Turnstile, mut req: Request, inner: F) -> Response
where
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let resource = format!("{} {}", req.method(), req.uri().path());
    let path = req.uri().path().to_owned();
    let x402_body = gate
        .x402_requirements(&path)
        .map(|requirements| X402Gate::required_body(vec![requirements], "payment required"));

    // x402 rail: an `X-PAYMENT` header on a gate that speaks x402.
    let x402_payment = req
        .headers()
        .get(HEADER_PAYMENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let (Some(payment), Some(x402), Some(body)) = (x402_payment, gate.x402(), x402_body.as_ref()) {
        return gate_x402(gate, x402, body, &resource, &payment, req, inner).await;
    }

    let authorization = req
        .headers()
        .get(HEADER_AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());
    let request_id = authorization
        .as_deref()
        .and_then(|encoded| aitp_client::codec::decode_authorization(encoded).ok())
        .map(|authorization| authorization.nonce);

    match gate.admit(authorization.as_deref(), &resource).await {
        Admit::Challenge(challenge) => response::challenge_with(&challenge, x402_body.as_ref()),
        Admit::Rejected(error) => match gate.challenge(&resource).await {
            Ok(fresh) => response::rejected_with(&error, &fresh, x402_body.as_ref()),
            // The nonce store is unavailable, so we cannot mint a fresh challenge.
            // Still fail closed with a `402` carrying the reason.
            Err(_) => response::unavailable(&error),
        },
        Admit::Admitted(settled) => {
            let quoted = settled.quoted();
            let reported = Arc::new(Mutex::new(None));
            req.extensions_mut()
                .insert(PaidContext::new(reported.clone(), quoted));

            let response = inner(req).await;
            let succeeded = response.status().is_success();

            // Success-only: capture what the handler reported (or the full quoted
            // price if it reported nothing); release on any non-2xx.
            let actual = reported
                .lock()
                .map(|g| *g)
                .unwrap_or(None)
                .unwrap_or(quoted);

            if succeeded {
                match settled.capture(actual).await {
                    Ok(receipt) => response::attach_request_id(
                        response::attach_receipt(response, receipt),
                        request_id.as_deref(),
                    ),
                    Err(error) => response::settlement_unavailable(&error, request_id.as_deref()),
                }
            } else {
                let response = match settled.release().await {
                    Ok(()) => response,
                    Err(error) => response::mark_release_pending(response, &error),
                };
                response::attach_request_id(response, request_id.as_deref())
            }
        }
    }
}

/// The x402 path: verify `X-PAYMENT` through the facilitator, run the handler,
/// settle on-chain only when it succeeded (success-only), and attach
/// `X-PAYMENT-RESPONSE`. The handler still receives a [`PaidContext`]; x402
/// `exact` moves the quoted amount, so a lower reported usage is not applied.
async fn gate_x402<F, Fut>(
    gate: &Turnstile,
    x402: &X402Gate,
    body: &turnstile_core::x402::PaymentRequiredBody,
    resource: &str,
    payment: &str,
    mut req: Request,
    inner: F,
) -> Response
where
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let Some(requirements) = body.accepts.first().cloned() else {
        return response::x402_unavailable("unexpected_verify_error", "no x402 requirements");
    };
    let admission: X402Admission = match x402.verify(payment, &requirements).await {
        Ok(admission) => admission,
        Err(rejection) if rejection.retryable => {
            return response::x402_unavailable(&rejection.code, &rejection.message)
        }
        Err(rejection) => {
            let refused = X402Gate::required_body(body.accepts.clone(), rejection.code);
            let fresh = gate.challenge(resource).await.ok();
            return response::x402_rejected(&refused, fresh.as_ref());
        }
    };

    let quoted = gate.price();
    req.extensions_mut()
        .insert(PaidContext::new(Arc::new(Mutex::new(None)), quoted));
    let response = inner(req).await;
    if !response.status().is_success() {
        x402.abandon(&admission, &format!("handler returned {}", response.status()))
            .await;
        return response;
    }
    match x402.settle(&admission).await {
        Ok(settled) => response::attach_x402_settlement(response, &settled),
        // The paid body is withheld: without a settled payment the caller
        // does not receive the resource.
        Err(rejection) if rejection.retryable => {
            response::x402_unavailable(&rejection.code, &rejection.message)
        }
        Err(rejection) => {
            let refused = X402Gate::required_body(body.accepts.clone(), rejection.code);
            response::x402_rejected(&refused, None)
        }
    }
}

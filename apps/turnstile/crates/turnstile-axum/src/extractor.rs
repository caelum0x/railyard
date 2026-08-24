//! The [`PaidRequest`] extractor: the handler's handle to a gated request.
//!
//! The [`admit`](crate::middleware::admit) middleware inserts a [`PaidContext`]
//! into request extensions after a successful verify. The handler pulls it out
//! via [`PaidRequest`] to learn the quoted price and to REPORT the actual usage
//! to bill — the middleware captures that reported amount after the handler
//! returns (success-only). If the handler reports nothing, the middleware
//! captures the full quoted price.

use std::sync::{Arc, Mutex};

use aitp_client::AmountMicros;
use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Shared settlement context bridged from the middleware to the handler.
///
/// Cloneable and cheap: the `reported` cell is shared (the handler writes, the
/// middleware reads after the inner handler returns).
#[derive(Clone)]
pub struct PaidContext {
    reported: Arc<Mutex<Option<AmountMicros>>>,
    price: AmountMicros,
}

impl PaidContext {
    #[must_use]
    pub fn new(reported: Arc<Mutex<Option<AmountMicros>>>, price: AmountMicros) -> Self {
        Self { reported, price }
    }
}

/// Handler-facing view of a paid request.
pub struct PaidRequest {
    ctx: PaidContext,
}

impl PaidRequest {
    /// The price quoted (and reserved via `aitp-hold`) for this call.
    #[must_use]
    pub fn price(&self) -> AmountMicros {
        self.ctx.price
    }

    /// Report the ACTUAL amount to bill (success-only capture). Must be `<=` the
    /// quoted price to stay within the reserved hold. If never called, the
    /// middleware captures the full quoted price on success.
    pub fn report_actual(&self, actual: AmountMicros) {
        if let Ok(mut guard) = self.ctx.reported.lock() {
            *guard = Some(actual);
        }
    }
}

#[async_trait]
impl<S> FromRequestParts<S> for PaidRequest
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<PaidContext>()
            .cloned()
            .map(|ctx| PaidRequest { ctx })
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "route not gated by turnstile\n",
                )
                    .into_response()
            })
    }
}

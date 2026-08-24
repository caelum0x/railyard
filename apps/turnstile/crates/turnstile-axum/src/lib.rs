//! Drop-in axum integration for [`turnstile_core`].
//!
//! Gate any route in a few lines:
//!
//! ```ignore
//! use std::sync::Arc;
//! use axum::{routing::post, Router, middleware::from_fn_with_state};
//! use turnstile_axum::{admit, PaidRequest};
//!
//! let gate: Arc<turnstile_core::Turnstile> = /* wired from config */;
//! let app = Router::new().route(
//!     "/v1/echo",
//!     post(handler).route_layer(from_fn_with_state(gate.clone(), admit)),
//! );
//!
//! async fn handler(paid: PaidRequest, body: String) -> String {
//!     paid.report_actual(/* actual usage */);
//!     body
//! }
//! ```
//!
//! The [`admit`](middleware::admit) middleware runs
//! [`Turnstile::admit`](turnstile_core::Turnstile::admit): on an unpaid/invalid
//! request it short-circuits a `402` carrying the `AITP-Challenge` +
//! `AITP-Version` headers; on a verified request it runs the inner handler, then
//! captures the actual amount the handler reported (via [`PaidRequest`]) on
//! success or releases the hold on failure, attaching `AITP-Receipt` on capture.

pub mod extractor;
pub mod middleware;
pub mod response;

pub use extractor::{PaidContext, PaidRequest};
pub use middleware::admit;

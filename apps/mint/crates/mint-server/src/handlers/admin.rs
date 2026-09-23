//! The operator console: one static page served at `GET /admin`. It carries no
//! data and no secrets; every panel calls the regular `/v1/*` API with the
//! operator-entered `X-API-Key`, so it is authorized exactly like the API.

use axum::http::header;
use axum::response::{IntoResponse, Response};

const ADMIN_HTML: &str = include_str!("../../assets/admin.html");

/// `GET /admin` → the operator console.
pub async fn console() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                 connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'",
            ),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        ADMIN_HTML,
    )
        .into_response()
}

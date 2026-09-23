//! Map domain errors onto GraphQL errors with a stable machine-readable `code`
//! extension, never leaking database internals to clients.

use async_graphql::{Error, ErrorExtensions};
use rithub_core::CoreError;

/// Convert a [`CoreError`] into a GraphQL [`Error`], attaching a `code`
/// extension (`NOT_FOUND`, `CONFLICT`, `BAD_REQUEST`, `INTERNAL`). Database
/// errors are collapsed to a generic message — their detail is logged upstream,
/// never returned.
pub fn core_error(err: CoreError) -> Error {
    let (code, message) = match &err {
        CoreError::InvalidName { .. } => ("BAD_REQUEST", err.to_string()),
        CoreError::NotFound { .. } => ("NOT_FOUND", err.to_string()),
        CoreError::Conflict { .. } => ("CONFLICT", err.to_string()),
        CoreError::Database(_) => ("INTERNAL", "internal server error".to_string()),
    };
    Error::new(message).extend_with(|_, e| e.set("code", code))
}

/// A `403`-equivalent authorization failure.
pub fn forbidden() -> Error {
    Error::new("forbidden").extend_with(|_, e| e.set("code", "FORBIDDEN"))
}

/// A `401`-equivalent: the operation requires an authenticated user.
pub fn unauthenticated() -> Error {
    Error::new("authentication required").extend_with(|_, e| e.set("code", "UNAUTHENTICATED"))
}

/// A `400`-equivalent client error with a caller-safe message.
pub fn bad_request(message: impl Into<String>) -> Error {
    Error::new(message.into()).extend_with(|_, e| e.set("code", "BAD_REQUEST"))
}

/// Convert a service-layer failure (already client-safe) into a GraphQL error.
pub fn service_error(err: crate::services::ServiceError) -> Error {
    let code = err.code;
    Error::new(err.message).extend_with(move |_, e| e.set("code", code))
}

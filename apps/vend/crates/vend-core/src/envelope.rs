//! The API envelope: `{success:true,data}` on success,
//! `{success:false,error:{code,message}}` on failure.

use serde::Serialize;

use crate::error::VendError;

/// A successful response body: `{"success":true,"data":...}`.
#[derive(Debug, Clone, Serialize)]
pub struct SuccessEnvelope<T: Serialize> {
    /// Always `true`.
    pub success: bool,
    /// The response payload.
    pub data: T,
}

impl<T: Serialize> SuccessEnvelope<T> {
    /// Wrap a payload in a success envelope.
    pub fn new(data: T) -> Self {
        SuccessEnvelope {
            success: true,
            data,
        }
    }
}

/// The `error` object inside a failure envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    /// Stable snake_case error code.
    pub code: String,
    /// Client-safe human message.
    pub message: String,
}

/// A failure response body: `{"success":false,"error":{...}}`.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    /// Always `false`.
    pub success: bool,
    /// The error object.
    pub error: ErrorBody,
}

impl ErrorEnvelope {
    /// Build a failure envelope from a [`VendError`], using its stable code and
    /// client-safe message.
    pub fn from_error(err: &VendError) -> Self {
        ErrorEnvelope {
            success: false,
            error: ErrorBody {
                code: err.code().to_string(),
                message: err.client_message(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_shape() {
        let json = serde_json::to_string(&SuccessEnvelope::new(7)).unwrap();
        assert_eq!(json, r#"{"success":true,"data":7}"#);
    }

    #[test]
    fn error_envelope_shape() {
        let env = ErrorEnvelope::from_error(&VendError::InsufficientFunds);
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""success":false"#));
        assert!(json.contains(r#""code":"insufficient_funds""#));
    }
}

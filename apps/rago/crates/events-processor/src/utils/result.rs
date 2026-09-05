//! Faithful port of `utils/result.go` — the generic `Result[T]` carried through
//! the whole pipeline, with its `Retryable` / `Capture` flags and optional
//! `ErrorDetails{Code, Message}`.
//!
//! We keep the Go name `Result` (imported as `crate::utils::Result`); use
//! `std::result::Result` / `anyhow::Result` explicitly for the std type.

use super::errors::RagoError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDetails {
    pub code: String,
    pub message: String,
}

/// Erased view of a `Result<T>`, matching Go's `AnyResult` interface. Used by
/// the Sentry helpers and by the processor when it rebuilds a typed failure
/// from an untyped one.
pub trait AnyResult: Send + Sync {
    fn is_success(&self) -> bool;
    fn is_failure(&self) -> bool;
    fn error_cloned(&self) -> Option<RagoError>;
    fn error_msg(&self) -> String;
    fn error_code(&self) -> String;
    fn error_message(&self) -> String;
    fn is_capturable(&self) -> bool;
    fn is_retryable(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct Result<T> {
    value: Option<T>,
    err: Option<RagoError>,
    details: Option<ErrorDetails>,
    pub retryable: bool,
    pub capture: bool,
}

impl<T> Result<T> {
    // --- constructors (SuccessResult / FailedResult) ---

    pub fn success(value: T) -> Self {
        Result {
            value: Some(value),
            err: None,
            details: None,
            retryable: false,
            capture: false,
        }
    }

    pub fn failed(err: impl Into<RagoError>) -> Self {
        Result {
            value: None,
            err: Some(err.into()),
            details: None,
            capture: true,
            retryable: true,
        }
    }

    // --- state ---

    pub fn is_success(&self) -> bool {
        self.err.is_none()
    }

    pub fn is_failure(&self) -> bool {
        self.err.is_some()
    }

    /// Borrow the success value. Callers guard with `is_failure()` first, exactly
    /// like the Go code that only calls `.Value()` after checking `Failure()`.
    pub fn value(&self) -> &T {
        self.value
            .as_ref()
            .expect("Result::value() called on a failed result")
    }

    /// Consume and return the success value.
    pub fn into_value(self) -> T {
        self.value
            .expect("Result::into_value() called on a failed result")
    }

    /// `ValueOrPanic()`.
    pub fn value_or_panic(self) -> T {
        match self.value {
            Some(v) => v,
            None => panic!("{}", self.err.map(|e| e.to_string()).unwrap_or_default()),
        }
    }

    pub fn error(&self) -> Option<&RagoError> {
        self.err.as_ref()
    }

    pub fn into_error(self) -> Option<RagoError> {
        self.err
    }

    // --- builder flags (chainable, consume + return like Go value receivers) ---

    pub fn add_error_details(
        mut self,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        self.details = Some(ErrorDetails {
            code: code.into(),
            message: message.into(),
        });
        self
    }

    pub fn non_retryable(mut self) -> Self {
        self.retryable = false;
        self
    }

    pub fn non_capturable(mut self) -> Self {
        self.capture = false;
        self
    }

    pub fn error_details(&self) -> Option<&ErrorDetails> {
        self.details.as_ref()
    }

    // --- inherent accessors mirroring the Go methods / AnyResult trait, so
    //     callers don't need the trait in scope ---

    pub fn error_msg(&self) -> String {
        match &self.err {
            None => String::new(),
            Some(e) => e.to_string(),
        }
    }

    pub fn error_code(&self) -> String {
        self.details
            .as_ref()
            .map(|d| d.code.clone())
            .unwrap_or_default()
    }

    pub fn error_message(&self) -> String {
        self.details
            .as_ref()
            .map(|d| d.message.clone())
            .unwrap_or_default()
    }

    pub fn is_capturable(&self) -> bool {
        self.capture
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }
}

impl<T: Send + Sync> AnyResult for Result<T> {
    fn is_success(&self) -> bool {
        self.err.is_none()
    }

    fn is_failure(&self) -> bool {
        self.err.is_some()
    }

    fn error_cloned(&self) -> Option<RagoError> {
        self.err.clone()
    }

    fn error_msg(&self) -> String {
        match &self.err {
            None => String::new(),
            Some(e) => e.to_string(),
        }
    }

    fn error_code(&self) -> String {
        self.details
            .as_ref()
            .map(|d| d.code.clone())
            .unwrap_or_default()
    }

    fn error_message(&self) -> String {
        self.details
            .as_ref()
            .map(|d| d.message.clone())
            .unwrap_or_default()
    }

    fn is_capturable(&self) -> bool {
        self.capture
    }

    fn is_retryable(&self) -> bool {
        self.retryable
    }
}

/// `SuccessResult`.
pub fn success<T>(value: T) -> Result<T> {
    Result::success(value)
}

/// `FailedResult`.
pub fn failed<T>(err: impl Into<RagoError>) -> Result<T> {
    Result::failed(err)
}

/// `FailedBoolResult`.
pub fn failed_bool(err: impl Into<RagoError>) -> Result<bool> {
    Result::failed(err)
}

/// Rebuild a typed failure from any erased result, preserving flags — mirrors
/// the private `failedResult` / `failedMultiEventsResult` helpers.
pub fn failed_from<T>(
    r: &dyn AnyResult,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Result<T> {
    let err = r
        .error_cloned()
        .unwrap_or_else(|| RagoError::Message(String::new()));
    let mut out = Result::<T>::failed(err).add_error_details(code, message);
    out.retryable = r.is_retryable();
    out.capture = r.is_capturable();
    out
}

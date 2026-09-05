//! Port of `utils/error_tracker.go` — Sentry capture helpers.

use super::errors::RagoError;
use super::result::AnyResult;

/// `CaptureErrorResult`.
pub fn capture_error_result(err_result: &dyn AnyResult) {
    capture_error_result_with_extra(err_result, "", None);
}

/// `CaptureErrorResultWithExtra`.
pub fn capture_error_result_with_extra(
    err_result: &dyn AnyResult,
    extra_key: &str,
    extra_value: Option<serde_json::Value>,
) {
    let code = err_result.error_code();
    let message = err_result.error_message();
    let extra_key = extra_key.to_string();

    sentry::with_scope(
        |scope| {
            scope.set_extra("error_code", code.clone().into());
            scope.set_extra("error_message", message.clone().into());
            if !extra_key.is_empty() {
                if let Some(v) = &extra_value {
                    scope.set_extra(&extra_key, v.clone());
                }
            }
        },
        || {
            if let Some(err) = err_result.error_cloned() {
                capture_error(&err);
            }
        },
    );
}

/// `CaptureError`.
pub fn capture_error(err: &RagoError) {
    sentry::capture_error(err);
}

/// `LogAndPanic`.
pub fn log_and_panic(err: &RagoError, message: &str) -> ! {
    tracing::error!(error = %err, "{message}");
    capture_error(err);
    panic!("{}", err);
}

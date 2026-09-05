//! Port of the `github.com/getlago/lago-expression/expression-go` binding.
//!
//! The Go service links a C FFI shim (`expression-go`) that wraps the native
//! Rust `expression-core` engine. Rago uses `expression-core` **directly** — the
//! same engine, no FFI — so this is a thin, behaviour-identical reimplementation
//! of the exported `evaluate(expr, event_json) -> *string` function: parse the
//! expression, deserialize the event JSON, evaluate, and return the stringified
//! result, or `None` on any parse/deserialize/evaluate failure.

use expression_core::ExpressionParser;

/// Equivalent of `expression.Evaluate(input, event)`.
///
/// Returns `Some(result_string)` on success, or `None` when the expression
/// cannot be parsed, the event JSON cannot be deserialized, or evaluation fails
/// — exactly the null-returning branches of the Go FFI shim.
pub fn evaluate(input: &str, event_json: &str) -> Option<String> {
    let expr = ExpressionParser::parse_expression(input).ok()?;
    let event: expression_core::Event = serde_json::from_str(event_json).ok()?;
    let res = expr.evaluate(&event).ok()?;
    Some(res.to_string())
}

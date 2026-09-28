//! Shared helpers for adapters.

use serde_json::{Map, Value};
use switchyard_core::{AdapterError, CallCtx, HttpRequest, HttpResponse};

/// Send through egress with the standard User-Agent and the call's timeout.
pub async fn send(ctx: &CallCtx, mut req: HttpRequest) -> Result<HttpResponse, AdapterError> {
    req.timeout = ctx.timeout;
    if !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        req.headers.push(("user-agent".into(), ctx.user_agent()));
    }
    Ok(ctx.http.send(req).await?)
}

/// Send and require a 2xx JSON body; classify failures.
pub async fn send_json(ctx: &CallCtx, req: HttpRequest, bill_on_4xx: bool) -> Result<(u16, Value), AdapterError> {
    let resp = send(ctx, req).await?;
    if !(200..300).contains(&resp.status) {
        return Err(AdapterError::from_status(resp.status, &resp.text(), bill_on_4xx));
    }
    let v = resp.json().map_err(|e| AdapterError::retryable(e).with_status(resp.status))?;
    Ok((resp.status, v))
}

/// Our products answer `{success, data}`; unwrap to `data` when present.
pub fn unwrap_envelope(v: Value) -> Value {
    match v {
        Value::Object(ref m) if m.contains_key("data") && (m.contains_key("success") || m.contains_key("ok") || m.contains_key("status")) => {
            m.get("data").cloned().unwrap_or(Value::Null)
        }
        other => other,
    }
}

/// The first array found at one of `keys`, or the value itself if it is an array.
pub fn first_array(v: &Value, keys: &[&str]) -> Vec<Value> {
    if let Value::Array(a) = v {
        return a.clone();
    }
    for k in keys {
        if let Some(Value::Array(a)) = v.get(*k) {
            return a.clone();
        }
    }
    Vec::new()
}

/// Required config value (own product base URLs).
pub fn config_url<'a>(ctx: &'a CallCtx, key: &str) -> Result<&'a str, AdapterError> {
    ctx.config(key).map(|s| s.trim_end_matches('/')).ok_or_else(|| AdapterError::fatal(format!("{key} is not configured on this Switchyard")))
}

pub fn str_field<'a>(input: &'a Value, key: &str) -> Result<&'a str, AdapterError> {
    input.get(key).and_then(Value::as_str).ok_or_else(|| AdapterError::rejected(format!("`{key}` is required")))
}

/// Copy `(from, to)` input fields that are present into a new object.
pub fn pick(input: &Value, pairs: &[(&str, &str)]) -> Value {
    let mut m = Map::new();
    for (from, to) in pairs {
        if let Some(v) = input.get(*from).filter(|v| !v.is_null()) {
            m.insert(to.to_string(), v.clone());
        }
    }
    Value::Object(m)
}

/// Attach the call's credential as a bearer token (own products).
pub fn with_bearer(ctx: &CallCtx, req: HttpRequest) -> Result<HttpRequest, AdapterError> {
    match &ctx.credential {
        Some(c) => Ok(req.secret_header("authorization", format!("Bearer {}", c.expose()))),
        None => Err(AdapterError::fatal("no credential available for this provider; store one with `sy cred add`")),
    }
}

pub fn host_of(u: &str) -> Result<String, AdapterError> {
    url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_string)).ok_or_else(|| AdapterError::rejected(format!("invalid URL {u}")))
}

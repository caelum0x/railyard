//! Request parameters for trace reads: scope, window, page size and cursor.

use serde::Deserialize;
use uuid::Uuid;

/// Default page size for `GET /relay/traces`.
pub const DEFAULT_LIMIT: i64 = 50;
/// Largest page size a caller may ask for.
pub const MAX_LIMIT: i64 = 200;
/// Default look-back window (7 days). Bounds partition scans.
pub const DEFAULT_WINDOW_HOURS: i64 = 24 * 7;
/// Largest look-back window: the 90-day metadata retention of OD-RY2.
pub const MAX_WINDOW_HOURS: i64 = 24 * 90;

/// Which API keys a read may see. Built by the gateway from the authenticated
/// principal; never from user input alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceScope {
    /// Only traces tagged with this API key public id.
    Key(String),
    /// Operator scope: every key, optionally narrowed to one key.
    All { key: Option<String> },
}

impl TraceScope {
    /// The key filter to apply, if any.
    pub fn key_filter(&self) -> Option<&str> {
        match self {
            TraceScope::Key(key) => Some(key.as_str()),
            TraceScope::All { key } => key.as_deref(),
        }
    }
}

/// Keyset cursor: the `(created_at, id)` of the last row of the previous page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceCursor {
    pub created_at_us: i64,
    pub id: Uuid,
}

impl TraceCursor {
    /// Parse `<created_at_us>:<uuid>`.
    pub fn parse(raw: &str) -> Option<Self> {
        let (ms, id) = raw.trim().split_once(':')?;
        Some(Self {
            created_at_us: ms.parse().ok()?,
            id: Uuid::parse_str(id).ok()?,
        })
    }

    /// Render as `<created_at_us>:<uuid>`.
    pub fn render(&self) -> String {
        format!("{}:{}", self.created_at_us, self.id)
    }
}

/// Raw query string of `GET /relay/traces` (and `scope`/`key` on the detail).
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RawTraceQuery {
    /// `own` (default) or `all` (operator keys only).
    #[serde(default)]
    pub scope: Option<String>,
    /// Operator scope only: narrow to one API key public id.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub window_hours: Option<i64>,
    #[serde(default)]
    pub before: Option<String>,
    /// Narrow to one function name.
    #[serde(default)]
    pub function: Option<String>,
}

impl RawTraceQuery {
    /// Whether the caller asked for the operator-wide scope.
    pub fn wants_all(&self) -> bool {
        self.scope.as_deref().map(str::trim) == Some("all")
    }
}

/// Validated list parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceListParams {
    pub limit: i64,
    pub window_hours: i64,
    pub before: Option<TraceCursor>,
    pub function_name: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TraceQueryError {
    #[error("invalid trace cursor; pass the `next_before` value from the previous page")]
    InvalidCursor,
    #[error("`limit` must be between 1 and {MAX_LIMIT}")]
    InvalidLimit,
    #[error("`window_hours` must be between 1 and {MAX_WINDOW_HOURS}")]
    InvalidWindow,
    #[error("`key` is only accepted with `scope=all` (operator keys)")]
    KeyFilterNeedsOperator,
}

impl TraceListParams {
    /// Validate a raw query. Out-of-range values are rejected, not clamped, so
    /// a caller never silently gets a different window than it asked for.
    pub fn from_raw(raw: &RawTraceQuery) -> Result<Self, TraceQueryError> {
        let limit = raw.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(TraceQueryError::InvalidLimit);
        }
        let window_hours = raw.window_hours.unwrap_or(DEFAULT_WINDOW_HOURS);
        if !(1..=MAX_WINDOW_HOURS).contains(&window_hours) {
            return Err(TraceQueryError::InvalidWindow);
        }
        let before = match raw.before.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => Some(TraceCursor::parse(value).ok_or(TraceQueryError::InvalidCursor)?),
        };
        let function_name = raw
            .function
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Ok(Self {
            limit,
            window_hours,
            before,
            function_name,
        })
    }
}

/// Build the scope from the authenticated principal and the raw query.
/// `is_operator` is the gateway's operator check for this principal.
pub fn scope_for(
    principal: &str,
    is_operator: bool,
    raw: &RawTraceQuery,
) -> Result<TraceScope, TraceQueryError> {
    let key = raw
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    if raw.wants_all() && is_operator {
        return Ok(TraceScope::All { key });
    }
    if key.is_some() {
        return Err(TraceQueryError::KeyFilterNeedsOperator);
    }
    Ok(TraceScope::Key(principal.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply() {
        let params = TraceListParams::from_raw(&RawTraceQuery::default()).unwrap();
        assert_eq!(params.limit, DEFAULT_LIMIT);
        assert_eq!(params.window_hours, DEFAULT_WINDOW_HOURS);
        assert_eq!(params.before, None);
        assert_eq!(params.function_name, None);
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let raw = RawTraceQuery {
            limit: Some(0),
            ..Default::default()
        };
        assert_eq!(
            TraceListParams::from_raw(&raw),
            Err(TraceQueryError::InvalidLimit)
        );
        let raw = RawTraceQuery {
            limit: Some(MAX_LIMIT + 1),
            ..Default::default()
        };
        assert_eq!(
            TraceListParams::from_raw(&raw),
            Err(TraceQueryError::InvalidLimit)
        );
        let raw = RawTraceQuery {
            window_hours: Some(MAX_WINDOW_HOURS + 1),
            ..Default::default()
        };
        assert_eq!(
            TraceListParams::from_raw(&raw),
            Err(TraceQueryError::InvalidWindow)
        );
        let raw = RawTraceQuery {
            before: Some("nope".into()),
            ..Default::default()
        };
        assert_eq!(
            TraceListParams::from_raw(&raw),
            Err(TraceQueryError::InvalidCursor)
        );
    }

    #[test]
    fn cursor_round_trips() {
        let cursor = TraceCursor {
            created_at_us: 1_790_000_000_000_000,
            id: Uuid::now_v7(),
        };
        assert_eq!(TraceCursor::parse(&cursor.render()), Some(cursor));
        assert_eq!(TraceCursor::parse("12:not-a-uuid"), None);
    }

    #[test]
    fn non_operator_is_always_scoped_to_own_key() {
        let raw = RawTraceQuery {
            scope: Some("all".into()),
            ..Default::default()
        };
        assert_eq!(
            scope_for("key-a", false, &raw),
            Ok(TraceScope::Key("key-a".into()))
        );
        let raw = RawTraceQuery {
            key: Some("key-b".into()),
            ..Default::default()
        };
        assert_eq!(
            scope_for("key-a", false, &raw),
            Err(TraceQueryError::KeyFilterNeedsOperator)
        );
    }

    #[test]
    fn operator_may_see_all_or_one_key() {
        let raw = RawTraceQuery {
            scope: Some("all".into()),
            key: Some("key-b".into()),
            ..Default::default()
        };
        let scope = scope_for("op", true, &raw).unwrap();
        assert_eq!(scope.key_filter(), Some("key-b"));
        let raw = RawTraceQuery {
            scope: Some("all".into()),
            ..Default::default()
        };
        assert_eq!(
            scope_for("op", true, &raw),
            Ok(TraceScope::All { key: None })
        );
        // Without `scope=all` an operator still reads its own key.
        assert_eq!(
            scope_for("op", true, &RawTraceQuery::default()),
            Ok(TraceScope::Key("op".into()))
        );
    }
}

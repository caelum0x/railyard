//! Port of `utils/string.go` — `StringArray`, a `[]string` that round-trips as
//! a JSON array and can also be scanned from a PostgreSQL `text[]` literal.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StringArray(pub Vec<String>);

impl StringArray {
    pub fn new() -> Self {
        StringArray(Vec::new())
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<String> {
        self.0
    }

    /// Scan a value coming from the database, mirroring `(*StringArray).Scan`:
    /// try JSON first, then fall back to the PostgreSQL array format.
    pub fn scan_db_bytes(bytes: &[u8]) -> Self {
        if let Ok(arr) = serde_json::from_slice::<Vec<String>>(bytes) {
            return StringArray(arr);
        }
        StringArray(parse_postgres_array(&String::from_utf8_lossy(bytes)))
    }
}

impl From<Vec<String>> for StringArray {
    fn from(v: Vec<String>) -> Self {
        StringArray(v)
    }
}

impl From<StringArray> for Vec<String> {
    fn from(a: StringArray) -> Self {
        a.0
    }
}

/// Reproduce Go's `fmt.Sprintf("%v", value)` for a JSON value read from an
/// event's `properties` map (where JSON numbers arrive as `float64`). Used by
/// filter matching, value extraction and pricing-group-key enrichment so the
/// stringified output matches the Go service.
pub fn go_sprintf_v(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "<nil>".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                // Go prints float64 with its shortest round-tripping form.
                format!("{f}")
            } else {
                n.to_string()
            }
        }
        // Arrays/objects are not expected for these keys; fall back to JSON.
        other => other.to_string(),
    }
}

/// Port of `parsePostgresArray`: `{a, b, c}` -> `["a", "b", "c"]`.
pub fn parse_postgres_array(s: &str) -> Vec<String> {
    let s = s.trim();
    if s == "{}" || s.is_empty() {
        return Vec::new();
    }
    let s = s.strip_prefix('{').unwrap_or(s);
    let s = s.strip_suffix('}').unwrap_or(s);

    s.split(',')
        .filter_map(|part| {
            let part = part.trim().trim_matches('"');
            if part.is_empty() {
                None
            } else {
                Some(part.to_string())
            }
        })
        .collect()
}

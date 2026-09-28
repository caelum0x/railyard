//! Output mapping: our own small, declarative JSON-to-JSON language.
//!
//! A mapping spec is JSON:
//! - a string starting with `/` (or the empty string) is a JSON Pointer into
//!   the source; a `*` segment maps over an array (`/items/*/name`);
//! - a string starting with `=` is a literal (`"=open-meteo"`);
//! - `{ "$each": "/results", "$map": { ... } }` maps every array item with the
//!   inner spec, pointers relative to the item; `"$limit": N` truncates;
//! - `{ "$first": ["/a", "/b"] }` takes the first non-null pointer;
//! - any other object maps key by key; numbers, bools and null are literals.
//!
//! A missing pointer yields `null`.

use serde_json::{Map, Value};

pub fn apply(spec: &Value, source: &Value) -> Value {
    match spec {
        Value::String(s) if s.is_empty() || s.starts_with('/') => pointer(source, s),
        Value::String(s) if s.starts_with('=') => Value::String(s[1..].to_string()),
        Value::String(s) => Value::String(s.clone()),
        Value::Object(obj) => {
            if let Some(each) = obj.get("$each").and_then(Value::as_str) {
                let items = pointer(source, each);
                let inner = obj.get("$map").cloned().unwrap_or(Value::String(String::new()));
                let limit = obj.get("$limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(usize::MAX);
                return match items {
                    Value::Array(arr) => Value::Array(arr.iter().take(limit).map(|item| apply(&inner, item)).collect()),
                    Value::Null => Value::Array(vec![]),
                    single => Value::Array(vec![apply(&inner, &single)]),
                };
            }
            if let Some(Value::Array(candidates)) = obj.get("$first") {
                return candidates.iter().map(|c| apply(c, source)).find(|v| !v.is_null()).unwrap_or(Value::Null);
            }
            let mut out = Map::new();
            for (k, v) in obj {
                out.insert(k.clone(), apply(v, source));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// JSON Pointer with `*` wildcard support.
pub fn pointer(source: &Value, ptr: &str) -> Value {
    if ptr.is_empty() {
        return source.clone();
    }
    if let Some(idx) = ptr.find("/*") {
        let (head, rest) = ptr.split_at(idx);
        let tail = &rest[2..];
        return match pointer(source, head) {
            Value::Array(items) => Value::Array(items.iter().map(|i| pointer(i, tail)).collect()),
            _ => Value::Null,
        };
    }
    source.pointer(ptr).cloned().unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_each_with_relative_pointers_and_limit() {
        let src = json!({"web": {"results": [
            {"title": "A", "url": "https://a", "description": "da"},
            {"title": "B", "url": "https://b", "description": "db"},
            {"title": "C", "url": "https://c", "description": "dc"}
        ]}});
        let spec = json!({"results": {"$each": "/web/results", "$limit": 2, "$map": {"title": "/title", "url": "/url", "snippet": "/description"}}, "source": "=brave"});
        assert_eq!(
            apply(&spec, &src),
            json!({"results": [{"title": "A", "url": "https://a", "snippet": "da"}, {"title": "B", "url": "https://b", "snippet": "db"}], "source": "brave"})
        );
    }

    #[test]
    fn wildcard_and_first_and_missing() {
        let src = json!({"authors": [{"name": "x"}, {"name": "y"}], "b": 2});
        assert_eq!(pointer(&src, "/authors/*/name"), json!(["x", "y"]));
        assert_eq!(apply(&json!({"$first": ["/a", "/b"]}), &src), json!(2));
        assert_eq!(apply(&json!("/nope"), &src), Value::Null);
        assert_eq!(apply(&json!({"$each": "/nope", "$map": "/x"}), &src), json!([]));
    }
}

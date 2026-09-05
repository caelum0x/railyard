//! Port of `utils/json.go` — `UnmarshalNestedJSON`.
//!
//! Go inspects struct tags at runtime: a tag like `properties.pricing_group_keys`
//! lifts `raw["properties"]["pricing_group_keys"]` into the field, while plain
//! tags read the top-level key. Rust has no runtime tag reflection, so a type
//! declares its dotted tags via [`NestedJson`]; we lift those nested values to
//! the top level under their literal (dotted) key, then let serde — which
//! renames the field to that same literal key — finish the job. This reproduces
//! the Go behaviour exactly for the models that use it (Charge, ChargeFilter).

use serde::de::DeserializeOwned;

use super::errors::RagoError;
use super::result::Result;

/// Types deserialized from Debezium CDC payloads implement this to declare the
/// `parent.child` JSON tags that must be lifted from a nested object.
pub trait NestedJson {
    /// Each entry is a `"parent.child"` literal tag.
    const DOTTED_KEYS: &'static [&'static str];
}

/// `UnmarshalNestedJSON(data, v)`.
pub fn unmarshal_nested_json<T: DeserializeOwned + NestedJson>(data: &[u8]) -> Result<T> {
    let mut raw: serde_json::Value = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(e) => return Result::failed(RagoError::from(e)),
    };

    if let serde_json::Value::Object(map) = &mut raw {
        for dotted in T::DOTTED_KEYS {
            if let Some((parent, child)) = dotted.split_once('.') {
                let lifted = map
                    .get(parent)
                    .and_then(|p| p.as_object())
                    .and_then(|p| p.get(child))
                    .cloned();
                if let Some(v) = lifted {
                    if !v.is_null() {
                        map.insert((*dotted).to_string(), v);
                    }
                }
            }
        }
    }

    match serde_json::from_value::<T>(raw) {
        Ok(v) => Result::success(v),
        Err(e) => Result::failed(RagoError::from(e)),
    }
}

//! REST `charge.filters` — parse, persist and serialize the filters of a plan
//! charge (`ChargeFilters::CreateOrUpdateBatchService` input shape):
//!
//! ```json
//! "filters": [
//!   { "invoice_display_name": "EU calls",
//!     "properties": { "amount": "0.02" },
//!     "values": { "region": ["eu-west", "eu-central"] } }
//! ]
//! ```

use rago_core::models::Charge;
use rago_db::charge_filters::{ChargeFilterRow, ChargeFilterSpec};
use serde_json::{json, Map, Value};

use crate::error::{ApiError, ApiResult};
use crate::serializers as ser;
use crate::state::AppState;

/// Upper bound on filters per charge (keeps one charge's fee fan-out sane).
const MAX_FILTERS: usize = 50;

fn invalid(code: &str) -> ApiError {
    ApiError::validation("filters", code)
}

/// `values`: a non-empty object of non-empty string arrays.
fn parse_values(raw: Option<&Value>) -> ApiResult<Value> {
    let obj = raw.and_then(Value::as_object).filter(|o| !o.is_empty()).ok_or_else(|| invalid("values_are_mandatory"))?;
    let mut out = Map::new();
    for (key, vals) in obj {
        let key = key.trim();
        if key.is_empty() {
            return Err(invalid("value_key_is_blank"));
        }
        let list: Vec<String> = vals
            .as_array()
            .ok_or_else(|| invalid("values_must_be_arrays"))?
            .iter()
            .map(|v| match v {
                Value::String(s) => s.trim().to_string(),
                other => other.to_string(),
            })
            .filter(|v| !v.is_empty())
            .collect();
        if list.is_empty() {
            return Err(invalid("values_are_mandatory"));
        }
        out.insert(key.to_string(), json!(list));
    }
    Ok(Value::Object(out))
}

/// The `filters` array of a charge payload; `None` when the key is absent
/// (update: leave the filters unchanged), `Some(vec![])` clears them.
pub(crate) fn parse_filters(charge: &Value) -> ApiResult<Option<Vec<ChargeFilterSpec>>> {
    let Some(raw) = charge.get("filters") else { return Ok(None) };
    if raw.is_null() {
        return Ok(Some(Vec::new()));
    }
    let items = raw.as_array().ok_or_else(|| invalid("value_is_invalid"))?;
    if items.len() > MAX_FILTERS {
        return Err(invalid("too_many_filters"));
    }
    let specs = items
        .iter()
        .map(|f| {
            let obj = f.as_object().ok_or_else(|| invalid("value_is_invalid"))?;
            let properties = match obj.get("properties") {
                None | Some(Value::Null) => json!({}),
                Some(p @ Value::Object(_)) => p.clone(),
                Some(_) => return Err(invalid("properties_must_be_an_object")),
            };
            Ok(ChargeFilterSpec {
                invoice_display_name: obj
                    .get("invoice_display_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                properties,
                values: parse_values(obj.get("values"))?,
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Some(specs))
}

/// `filters` of the charge serializer.
pub(crate) fn filters_json(rows: &[ChargeFilterRow]) -> Value {
    json!(rows
        .iter()
        .map(|r| json!({
            "lago_id": r.id,
            "invoice_display_name": r.spec.invoice_display_name,
            "properties": r.spec.properties,
            "values": r.spec.values,
        }))
        .collect::<Vec<_>>())
}

/// A charge serialized with its filters.
pub(crate) async fn charge_json(st: &AppState, charge: &Charge) -> ApiResult<Value> {
    let mut out = ser::charge(charge);
    out["filters"] = filters_json(&st.db.charge_filter_rows(charge.id).await?);
    Ok(out)
}

/// Every charge of a plan serialized with its filters.
pub(crate) async fn charges_json(st: &AppState, charges: &[Charge]) -> ApiResult<Vec<Value>> {
    let mut out = Vec::with_capacity(charges.len());
    for c in charges {
        out.push(charge_json(st, c).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filters_and_rejects_empty_values() {
        let body = json!({ "filters": [{ "invoice_display_name": " EU ", "properties": { "amount": "1" }, "values": { "region": ["eu", ""] } }] });
        let specs = parse_filters(&body).unwrap().unwrap();
        assert_eq!(specs[0].invoice_display_name.as_deref(), Some("EU"));
        assert_eq!(specs[0].values, json!({ "region": ["eu"] }));
        assert!(parse_filters(&json!({ "filters": [{ "values": { "region": [] } }] })).is_err());
        assert!(parse_filters(&json!({})).unwrap().is_none());
        assert_eq!(parse_filters(&json!({ "filters": null })).unwrap(), Some(vec![]));
    }
}

//! JSON Schema (2020-12) validation with JSON Pointer error locations.

use serde_json::Value;

/// A compiled schema.
pub struct Schema {
    validator: jsonschema::Validator,
}

impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Schema")
    }
}

impl Schema {
    pub fn compile(schema: &Value) -> Result<Self, String> {
        jsonschema::validator_for(schema).map(|validator| Schema { validator }).map_err(|e| e.to_string())
    }

    /// Validate; on failure return `(json_pointer, message)` pairs, pointer `""`
    /// meaning the root.
    pub fn validate(&self, instance: &Value) -> Result<(), Vec<(String, String)>> {
        let errors: Vec<(String, String)> =
            self.validator.iter_errors(instance).map(|e| (e.instance_path().to_string(), e.to_string())).collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Schema {
        Schema::compile(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "q": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["q"],
            "additionalProperties": false
        }))
        .unwrap()
    }

    #[test]
    fn valid_input_passes() {
        assert!(schema().validate(&json!({"q": "rust", "limit": 5})).is_ok());
    }

    #[test]
    fn errors_carry_json_pointers() {
        let errs = schema().validate(&json!({"q": "", "limit": 99, "tags": ["ok", 3]})).unwrap_err();
        let pointers: Vec<&str> = errs.iter().map(|(p, _)| p.as_str()).collect();
        assert!(pointers.contains(&"/q"), "{errs:?}");
        assert!(pointers.contains(&"/limit"), "{errs:?}");
        assert!(pointers.contains(&"/tags/1"), "{errs:?}");
    }

    #[test]
    fn missing_required_and_undeclared_fields_rejected() {
        let errs = schema().validate(&json!({"extra": true})).unwrap_err();
        assert!(errs.len() >= 2, "{errs:?}");
        assert!(errs.iter().any(|(_, m)| m.contains("q")));
    }

    #[test]
    fn bad_schema_fails_to_compile() {
        assert!(Schema::compile(&json!({"type": 12})).is_err());
    }
}

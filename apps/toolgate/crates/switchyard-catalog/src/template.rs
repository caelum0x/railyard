//! Request templating (minijinja). Templates see `input` and `config` only;
//! credentials are placed by the adapter, never through templates.

use minijinja::{Environment, Value as JValue};
use serde_json::{Map, Value};

fn env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_filter("pathseg", |s: String| -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
    });
    env.add_filter("join_csv", |v: JValue| -> String {
        match v.try_iter() {
            Ok(iter) => iter.map(|i| i.to_string()).collect::<Vec<_>>().join(","),
            Err(_) => v.to_string(),
        }
    });
    env
}

/// Render one string template.
pub fn render(template: &str, input: &Value, config: &Value) -> Result<String, String> {
    if !template.contains("{{") && !template.contains("{%") {
        return Ok(template.to_string());
    }
    let env = env();
    let ctx = minijinja::context! { input => JValue::from_serialize(input), config => JValue::from_serialize(config) };
    env.render_str(template, ctx).map(|s| if s == "none" { String::new() } else { s }).map_err(|e| e.to_string())
}

/// Render a JSON body: a string leaf `"$input/ptr"` copies the typed input value
/// (dropped when absent); other string leaves are templates.
pub fn render_body(body: &Value, input: &Value, config: &Value) -> Result<Value, String> {
    Ok(match body {
        Value::String(s) if s.starts_with("$input") => input.pointer(&s["$input".len()..]).cloned().unwrap_or(Value::Null),
        Value::String(s) => Value::String(render(s, input, config)?),
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                let r = render_body(v, input, config)?;
                if !r.is_null() {
                    out.insert(k.clone(), r);
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| render_body(v, input, config)).collect::<Result<_, _>>()?),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_input_and_config_with_defaults() {
        let input = json!({"lat": 52.52, "q": "Ada Lovelace", "n": null});
        let config = json!({"searxng_url": "https://search.example"});
        assert_eq!(render("{{ input.lat }}", &input, &config).unwrap(), "52.52");
        assert_eq!(render("{{ config.searxng_url }}/search", &input, &config).unwrap(), "https://search.example/search");
        assert_eq!(render("{{ input.q | pathseg }}", &input, &config).unwrap(), "Ada%20Lovelace");
        assert_eq!(render("{{ input.limit | default(10) }}", &input, &config).unwrap(), "10");
        assert_eq!(render("{{ input.missing }}", &input, &config).unwrap(), "");
    }

    #[test]
    fn body_keeps_types_and_drops_absent() {
        let input = json!({"to": ["a@x.io"], "n": 3});
        let body = json!({"to": "$input/to", "count": "$input/n", "cc": "$input/cc", "subject": "Hi {{ input.n }}"});
        assert_eq!(render_body(&body, &input, &json!({})).unwrap(), json!({"to": ["a@x.io"], "count": 3, "subject": "Hi 3"}));
    }
}

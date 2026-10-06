//! Which requests may be served from the cache, and how they split into a
//! scope (exact-match part) and a prompt (embedded part).

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// A chat request the cache may answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheableRequest {
    /// Hex SHA-256 of everything but the conversation.
    pub scope: String,
    /// The conversation as `role: text` lines.
    pub prompt: String,
}

/// Fields that do not change the answer and are left out of the scope.
const IGNORED: [&str; 5] = ["messages", "stream", "stream_options", "user", "metadata"];

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(p.get("text").and_then(Value::as_str)?.to_string()),
                    // Images, audio and files are not embedded: never cache them.
                    _ => return None,
                }
            }
            Some(out.join("\n"))
        }
        Value::Null => Some(String::new()),
        _ => None,
    }
}

/// An OpenAI-style chat completion body (`model` + `messages`), not streamed,
/// with text-only content. Anything else is not cacheable.
pub fn cacheable(body: &Value) -> Option<CacheableRequest> {
    let obj = body.as_object()?;
    obj.get("model")?.as_str()?;
    if obj.get("stream").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let messages = obj.get("messages")?.as_array()?;
    if messages.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(messages.len());
    for m in messages {
        let role = m.get("role").and_then(Value::as_str)?;
        if m.get("tool_calls").is_some() || role == "tool" {
            // Tool round-trips depend on live tool output.
            return None;
        }
        lines.push(format!("{role}: {}", content_text(m.get("content").unwrap_or(&Value::Null))?));
    }
    let scope_fields: Map<String, Value> = obj.iter().filter(|(k, _)| !IGNORED.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let roles: Vec<&str> = messages.iter().filter_map(|m| m.get("role").and_then(Value::as_str)).collect();
    let canonical = serde_json::to_string(&(Value::Object(sorted(scope_fields)), roles)).ok()?;
    let digest = Sha256::digest(canonical.as_bytes());
    let scope = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(CacheableRequest { scope, prompt: lines.join("\n") })
}

fn sorted(map: Map<String, Value>) -> Map<String, Value> {
    let mut pairs: Vec<(String, Value)> = map.into_iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scope_ignores_the_conversation_and_transport_fields() {
        let a = cacheable(&json!({"model": "m", "temperature": 0, "messages": [{"role": "user", "content": "hi"}], "user": "u1"})).unwrap();
        let b = cacheable(&json!({"temperature": 0, "model": "m", "messages": [{"role": "user", "content": "hello"}], "stream": false})).unwrap();
        assert_eq!(a.scope, b.scope);
        assert_eq!(a.prompt, "user: hi");
        let c = cacheable(&json!({"model": "m", "temperature": 1, "messages": [{"role": "user", "content": "hi"}]})).unwrap();
        assert_ne!(a.scope, c.scope, "sampling changes the scope");
        let d = cacheable(&json!({"model": "other", "temperature": 0, "messages": [{"role": "user", "content": "hi"}]})).unwrap();
        assert_ne!(a.scope, d.scope, "model changes the scope");
    }

    #[test]
    fn streams_tools_and_non_text_are_not_cacheable() {
        assert!(cacheable(&json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": "x"}]})).is_none());
        assert!(cacheable(&json!({"model": "m", "messages": [{"role": "tool", "content": "x"}]})).is_none());
        assert!(cacheable(&json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "u"}}]}]})).is_none());
        assert!(cacheable(&json!({"model": "m", "messages": []})).is_none());
        assert!(cacheable(&json!({"messages": [{"role": "user", "content": "x"}]})).is_none());
        let parts = cacheable(&json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}]})).unwrap();
        assert_eq!(parts.prompt, "user: a\nb");
    }
}

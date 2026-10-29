//! Which requests may be served from the cache, and how they split into a
//! scope (exact-match part) and a prompt (embedded part).

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// A chat request the cache may answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheableRequest {
    /// Hex SHA-256 of everything but the conversation's non-static turns:
    /// model/sampling/tools/response_format, AND every `system`/`developer`
    /// message verbatim. A system/developer prompt is typically a long,
    /// static persona (hundreds of tokens) dwarfing a user's few-word turn —
    /// it must match **exactly**, never fuzzily, and must never dominate the
    /// embedding used to compare the turns that actually vary per request.
    pub scope: String,
    /// The non-static turns (`user`/`assistant`/...) as `role: text` lines.
    /// This, not the system prompt, is what gets embedded for similarity.
    pub prompt: String,
}

/// Fields that do not change the answer and are left out of the scope.
const IGNORED: [&str; 5] = ["messages", "stream", "stream_options", "user", "metadata"];

/// Roles whose content is a static instruction, not a conversational turn:
/// folded into the exact-match `scope`, never into the embedded `prompt`.
fn is_static_role(role: &str) -> bool {
    role == "system" || role == "developer"
}

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
    let mut prompt_lines = Vec::with_capacity(messages.len());
    let mut system_lines = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str)?;
        if m.get("tool_calls").is_some() || role == "tool" {
            // Tool round-trips depend on live tool output.
            return None;
        }
        let text = content_text(m.get("content").unwrap_or(&Value::Null))?;
        if is_static_role(role) {
            system_lines.push(format!("{role}: {text}"));
        } else {
            prompt_lines.push(format!("{role}: {text}"));
        }
    }
    let scope_fields: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !IGNORED.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("role").and_then(Value::as_str))
        .collect();
    let canonical =
        serde_json::to_string(&(Value::Object(sorted(scope_fields)), roles, &system_lines)).ok()?;
    let digest = Sha256::digest(canonical.as_bytes());
    let scope = digest.iter().map(|b| format!("{b:02x}")).collect();
    // A request with only system/developer turns (no user content to embed)
    // falls back to embedding the static text itself, so it can still hit
    // its own exact re-submission; this never collides with a different
    // tenant/scope's traffic since `nearest` is already scoped by both.
    let prompt = if prompt_lines.is_empty() {
        system_lines.join("\n")
    } else {
        prompt_lines.join("\n")
    };
    Some(CacheableRequest { scope, prompt })
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
    fn a_long_system_prompt_is_excluded_from_the_embedded_prompt() {
        // Regression for the real false-cache-hit bug: a large static persona
        // (Kamiko's ~500-token system prompt) must not dominate the embedded
        // similarity of three completely different user questions.
        let persona = "You are Kamiko, a glitchy vtuber persona. ".repeat(40);
        let req = |q: &str| json!({"model": "m", "messages": [{"role": "system", "content": persona.clone()}, {"role": "user", "content": q}]});
        let sky = cacheable(&req("What color is the sky?")).unwrap();
        let joke = cacheable(&req("Tell me a joke about cats.")).unwrap();
        let movie = cacheable(&req("Recommend a movie.")).unwrap();
        // The embedded prompt never contains the system persona text.
        assert!(!sky.prompt.contains("Kamiko"));
        assert_eq!(sky.prompt, "user: What color is the sky?");
        assert_eq!(joke.prompt, "user: Tell me a joke about cats.");
        assert_eq!(movie.prompt, "user: Recommend a movie.");
        // Same persona -> same scope (exact match is still correct)...
        assert_eq!(sky.scope, joke.scope);
        assert_eq!(sky.scope, movie.scope);
        // ...but the three different questions no longer look identical, so
        // a real embedder sees genuinely different text, not the same
        // persona-dominated string three times.
        assert_ne!(sky.prompt, joke.prompt);
        assert_ne!(sky.prompt, movie.prompt);
    }

    #[test]
    fn a_different_system_prompt_changes_the_scope_even_with_the_same_question() {
        let a = cacheable(&json!({"model": "m", "messages": [{"role": "system", "content": "You are Kamiko."}, {"role": "user", "content": "hi"}]})).unwrap();
        let b = cacheable(&json!({"model": "m", "messages": [{"role": "system", "content": "You are a helpful assistant."}, {"role": "user", "content": "hi"}]})).unwrap();
        assert_ne!(
            a.scope, b.scope,
            "different personas must never share a cache entry"
        );
        assert_eq!(
            a.prompt, b.prompt,
            "the embedded prompt is persona-independent"
        );
    }

    #[test]
    fn short_or_absent_system_prompts_still_cache_normally() {
        // A tenant with no system prompt at all: unchanged behavior.
        let no_system =
            cacheable(&json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]}))
                .unwrap();
        assert_eq!(no_system.prompt, "user: hi");
        // A short system prompt: still excluded from the embedded prompt,
        // but a short one was never the source of the bug either way.
        let short_system =
            cacheable(&json!({"model": "m", "messages": [{"role": "system", "content": "Be concise."}, {"role": "user", "content": "hi"}]})).unwrap();
        assert_eq!(short_system.prompt, "user: hi");
        assert_ne!(
            no_system.scope, short_system.scope,
            "presence of a system prompt still affects scope"
        );
    }

    #[test]
    fn a_system_only_request_falls_back_to_embedding_itself() {
        let a = cacheable(&json!({"model": "m", "messages": [{"role": "system", "content": "Summarize the attached document."}]})).unwrap();
        assert_eq!(a.prompt, "system: Summarize the attached document.");
    }

    #[test]
    fn streams_tools_and_non_text_are_not_cacheable() {
        assert!(cacheable(
            &json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": "x"}]})
        )
        .is_none());
        assert!(
            cacheable(&json!({"model": "m", "messages": [{"role": "tool", "content": "x"}]}))
                .is_none()
        );
        assert!(cacheable(&json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "u"}}]}]})).is_none());
        assert!(cacheable(&json!({"model": "m", "messages": []})).is_none());
        assert!(cacheable(&json!({"messages": [{"role": "user", "content": "x"}]})).is_none());
        let parts = cacheable(&json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}]})).unwrap();
        assert_eq!(parts.prompt, "user: a\nb");
    }
}

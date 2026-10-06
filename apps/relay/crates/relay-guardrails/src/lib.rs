//! Relay guardrails: what a request may carry to a model provider.
//!
//! Applied to the prompt of every inference request (chat, completions,
//! responses, embeddings, native `/inference`), after authentication and
//! before spend caps, billing and the provider call, so a blocked request
//! never opens a hold or reaches a provider.
//!
//! * **PII**: e-mail addresses, phone numbers, payment cards (Luhn-checked),
//!   IBANs (mod-97 checked), US social security numbers and API secrets
//!   (`sk-…`, `AKIA…`, `ghp_…`, `xox…`, PEM private keys). Per policy they are
//!   redacted in place (`[REDACTED_EMAIL]`) or the request is refused.
//! * **Deny rules**: operator regular expressions; a match refuses the
//!   request with the rule's message.
//!
//! The policy comes from `RELAY_GUARDRAILS_JSON` (or a file named by
//! `RELAY_GUARDRAILS_PATH`); without either, guardrails are off.

pub mod middleware;

use std::borrow::Cow;
use std::collections::BTreeMap;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

/// What to do with personal data found in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PiiMode {
    /// Leave it.
    #[default]
    Off,
    /// Replace each match with `[REDACTED_<KIND>]` and send the request on.
    Redact,
    /// Refuse the request.
    Block,
}

/// A kind of personal or secret data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    Email,
    Phone,
    Card,
    Iban,
    Ssn,
    Secret,
}

impl Detector {
    pub const ALL: [Detector; 6] = [
        Detector::Secret,
        Detector::Card,
        Detector::Iban,
        Detector::Email,
        Detector::Ssn,
        Detector::Phone,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Detector::Email => "EMAIL",
            Detector::Phone => "PHONE",
            Detector::Card => "CARD",
            Detector::Iban => "IBAN",
            Detector::Ssn => "SSN",
            Detector::Secret => "SECRET",
        }
    }
}

/// An operator deny rule.
#[derive(Debug, Clone, Deserialize)]
pub struct DenyRuleSpec {
    pub pattern: String,
    /// Shown to the caller when the rule refuses a request.
    #[serde(default)]
    pub message: Option<String>,
}

/// The policy as configured (JSON).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PolicySpec {
    #[serde(default)]
    pub pii: PiiMode,
    /// Which detectors run (default: all of them).
    #[serde(default)]
    pub detectors: Option<Vec<Detector>>,
    #[serde(default)]
    pub deny: Vec<DenyRuleSpec>,
    /// Refuse prompts longer than this many characters (all text summed).
    #[serde(default)]
    pub max_prompt_chars: Option<usize>,
}

/// A configuration error, reported at startup.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("guardrails policy is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("deny rule {index} is not a valid regular expression: {source}")]
    Pattern { index: usize, source: regex::Error },
    #[error("reading the guardrails policy file: {0}")]
    Io(#[from] std::io::Error),
}

struct DenyRule {
    regex: Regex,
    message: String,
}

/// A compiled policy.
pub struct Policy {
    pii: PiiMode,
    detectors: Vec<(Detector, Regex)>,
    deny: Vec<DenyRule>,
    max_prompt_chars: Option<usize>,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("pii", &self.pii)
            .field(
                "detectors",
                &self.detectors.iter().map(|(d, _)| *d).collect::<Vec<_>>(),
            )
            .field("deny_rules", &self.deny.len())
            .field("max_prompt_chars", &self.max_prompt_chars)
            .finish()
    }
}

fn detector_regex(d: Detector) -> Regex {
    let pattern = match d {
        Detector::Email => r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b",
        // International (+ or 00 prefix) or North American formatted numbers;
        // bare digit runs are left alone (order numbers, amounts).
        Detector::Phone => {
            r"(?:(?:\+|\b00)\d{1,3}[\s.\-]?(?:\(?\d{1,4}\)?[\s.\-]?){2,5}\d{2,4}\b|(?:\(\d{3}\)\s?|\b\d{3}[\s.\-])\d{3}[\s.\-]\d{4}\b)"
        }
        Detector::Card => r"\b(?:\d[ \-]?){12,18}\d\b",
        Detector::Iban => r"(?i)\b[a-z]{2}\d{2}(?:[ ]?[a-z0-9]){11,30}\b",
        Detector::Ssn => r"\b\d{3}-\d{2}-\d{4}\b",
        Detector::Secret => {
            r"(?:\bsk-(?:proj-|ant-)?[A-Za-z0-9_\-]{20,}|\bAKIA[0-9A-Z]{16}\b|\bgh[pousr]_[A-Za-z0-9]{36,}\b|\bxox[abprs]-[A-Za-z0-9\-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----)"
        }
    };
    Regex::new(pattern).expect("built-in guardrail pattern")
}

/// Whether a candidate match is real (checksums cut false positives).
fn confirmed(d: Detector, m: &str) -> bool {
    let digits: String = m.chars().filter(char::is_ascii_digit).collect();
    match d {
        Detector::Card => (13..=19).contains(&digits.len()) && luhn(&digits),
        Detector::Phone => (10..=15).contains(&digits.len()) && !luhn_card_like(&digits),
        Detector::Iban => iban_valid(m),
        _ => true,
    }
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut v = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

/// A 13-19 digit Luhn-valid run is reported as a card, not a phone.
fn luhn_card_like(digits: &str) -> bool {
    (13..=19).contains(&digits.len()) && luhn(digits)
}

fn iban_valid(m: &str) -> bool {
    let s: String = m
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if !(15..=34).contains(&s.len()) || !s[..2].chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let chunk = if c.is_ascii_digit() {
            c.to_digit(10).unwrap_or(0).to_string()
        } else if c.is_ascii_alphabetic() {
            (c as u32 - 'A' as u32 + 10).to_string()
        } else {
            return false;
        };
        for d in chunk.chars() {
            rem = (rem * 10 + d.to_digit(10).unwrap_or(0)) % 97;
        }
    }
    rem == 1
}

/// What the policy decided for one request.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Send unchanged.
    Pass,
    /// Send this body instead; `counts` is the number of redactions per kind.
    Redacted {
        body: Value,
        counts: BTreeMap<&'static str, usize>,
    },
    /// Refuse; `code` is machine-readable, `message` safe to show.
    Blocked { code: &'static str, message: String },
}

/// Keys whose string values are not prompt text (they are never scanned or
/// rewritten).
const NON_CONTENT_KEYS: &[&str] = &[
    "model",
    "role",
    "type",
    "name",
    "id",
    "tool_call_id",
    "function_name",
    "variant_name",
    "episode_id",
    "encoding_format",
    "response_format",
    "user",
    "detail",
    "mime_type",
    "url",
    "image_url",
    "file_id",
];

impl Policy {
    /// Compile a policy.
    pub fn compile(spec: PolicySpec) -> Result<Self, PolicyError> {
        let chosen = spec.detectors.unwrap_or_else(|| Detector::ALL.to_vec());
        // Fixed order (secrets and cards before phones) so overlaps resolve
        // to the most specific kind.
        let detectors = Detector::ALL
            .iter()
            .copied()
            .filter(|d| chosen.contains(d))
            .map(|d| (d, detector_regex(d)))
            .collect();
        let deny = spec
            .deny
            .into_iter()
            .enumerate()
            .map(|(index, r)| {
                Ok(DenyRule {
                    regex: Regex::new(&r.pattern)
                        .map_err(|source| PolicyError::Pattern { index, source })?,
                    message: r.message.unwrap_or_else(|| {
                        "This request is not allowed by the gateway's policy.".into()
                    }),
                })
            })
            .collect::<Result<Vec<_>, PolicyError>>()?;
        Ok(Self {
            pii: spec.pii,
            detectors,
            deny,
            max_prompt_chars: spec.max_prompt_chars,
        })
    }

    /// Parse and compile a JSON policy.
    pub fn from_json(raw: &str) -> Result<Self, PolicyError> {
        Self::compile(serde_json::from_str(raw)?)
    }

    /// The policy from `RELAY_GUARDRAILS_JSON` or the file in
    /// `RELAY_GUARDRAILS_PATH`; `Ok(None)` when neither is set.
    pub fn from_env() -> Result<Option<Self>, PolicyError> {
        if let Ok(raw) = std::env::var("RELAY_GUARDRAILS_JSON") {
            if !raw.trim().is_empty() {
                return Self::from_json(&raw).map(Some);
            }
        }
        if let Ok(path) = std::env::var("RELAY_GUARDRAILS_PATH") {
            if !path.trim().is_empty() {
                return Self::from_json(&std::fs::read_to_string(path.trim())?).map(Some);
            }
        }
        Ok(None)
    }

    /// Whether this policy can change anything.
    pub fn is_active(&self) -> bool {
        self.pii != PiiMode::Off || !self.deny.is_empty() || self.max_prompt_chars.is_some()
    }

    /// Apply the policy to a JSON request body.
    pub fn check(&self, body: &Value) -> Verdict {
        let mut texts: Vec<&str> = Vec::new();
        collect_texts(body, None, &mut texts);
        if let Some(max) = self.max_prompt_chars {
            let total: usize = texts.iter().map(|t| t.chars().count()).sum();
            if total > max {
                return Verdict::Blocked {
                    code: "prompt_too_long",
                    message: format!(
                        "The prompt is {total} characters; this gateway accepts at most {max}."
                    ),
                };
            }
        }
        for rule in &self.deny {
            if texts.iter().any(|t| rule.regex.is_match(t)) {
                return Verdict::Blocked {
                    code: "denied_content",
                    message: rule.message.clone(),
                };
            }
        }
        if self.pii == PiiMode::Off {
            return Verdict::Pass;
        }
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for t in &texts {
            for (d, _) in self.redact_text(t).1 {
                *counts.entry(d.label()).or_default() += 1;
            }
        }
        if counts.is_empty() {
            return Verdict::Pass;
        }
        match self.pii {
            PiiMode::Block => {
                Verdict::Blocked {
                    code: "personal_data",
                    message: format!(
                    "The prompt contains personal or secret data ({}); remove it and send again.",
                    counts.keys().map(|k| k.to_ascii_lowercase()).collect::<Vec<_>>().join(", ")
                ),
                }
            }
            _ => {
                let mut out = body.clone();
                rewrite_texts(&mut out, None, &|t| self.redact_text(t).0);
                Verdict::Redacted { body: out, counts }
            }
        }
    }

    /// Redact one text; returns it and what was found.
    fn redact_text<'a>(&self, text: &'a str) -> (Cow<'a, str>, Vec<(Detector, String)>) {
        let mut out: Cow<'a, str> = Cow::Borrowed(text);
        let mut found = Vec::new();
        for (d, re) in &self.detectors {
            let current = out.to_string();
            let mut changed = false;
            let replaced = re.replace_all(&current, |caps: &regex::Captures<'_>| {
                let m = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
                if m.starts_with("[REDACTED_") || !confirmed(*d, m) {
                    return m.to_string();
                }
                changed = true;
                found.push((*d, m.to_string()));
                format!("[REDACTED_{}]", d.label())
            });
            if changed {
                out = Cow::Owned(replaced.into_owned());
            }
        }
        (out, found)
    }
}

fn collect_texts<'v>(v: &'v Value, key: Option<&str>, out: &mut Vec<&'v str>) {
    match v {
        Value::String(s) if !key.is_some_and(|k| NON_CONTENT_KEYS.contains(&k)) => out.push(s),
        Value::Array(items) => items.iter().for_each(|i| collect_texts(i, key, out)),
        Value::Object(map) => map.iter().for_each(|(k, v)| collect_texts(v, Some(k), out)),
        _ => {}
    }
}

fn rewrite_texts(v: &mut Value, key: Option<&str>, f: &dyn Fn(&str) -> Cow<'_, str>) {
    match v {
        Value::String(s) if !key.is_some_and(|k| NON_CONTENT_KEYS.contains(&k)) => {
            let new = f(s).into_owned();
            *s = new;
        }
        Value::Array(items) => items.iter_mut().for_each(|i| rewrite_texts(i, key, f)),
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                rewrite_texts(v, Some(k.as_str()), f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(raw: Value) -> Policy {
        Policy::from_json(&raw.to_string()).unwrap()
    }

    fn chat(text: &str) -> Value {
        json!({ "model": "openai::gpt-4o-mini", "messages": [{ "role": "user", "content": text }] })
    }

    #[test]
    fn redact_replaces_each_kind_and_keeps_the_rest_of_the_request() {
        let p = policy(json!({ "pii": "redact" }));
        let body = chat(
            "Mail ada@example.com, call +44 20 7946 0958, card 4111 1111 1111 1111, \
             IBAN GB82 WEST 1234 5698 7654 32, ssn 123-45-6789, key sk-proj-abcdefghijklmnopqrstuvwx",
        );
        let Verdict::Redacted { body, counts } = p.check(&body) else {
            panic!("expected redaction")
        };
        let text = body["messages"][0]["content"].as_str().unwrap();
        for kind in ["EMAIL", "PHONE", "CARD", "IBAN", "SSN", "SECRET"] {
            assert!(
                text.contains(&format!("[REDACTED_{kind}]")),
                "{kind} in {text}"
            );
            assert_eq!(counts.get(kind), Some(&1), "{kind}: {counts:?}");
        }
        assert!(!text.contains("ada@example.com") && !text.contains("4111"));
        assert_eq!(
            body["model"], "openai::gpt-4o-mini",
            "non-content fields are untouched"
        );
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn checksums_keep_ordinary_numbers() {
        let p = policy(json!({ "pii": "redact" }));
        // Not Luhn-valid, not a valid IBAN, a year range and an order number.
        let body = chat(
            "Order 4111 1111 1111 1112 shipped 2024-2026, ref GB00 WEST 1234 5698 7654 32, qty 12",
        );
        assert_eq!(p.check(&body), Verdict::Pass);
    }

    #[test]
    fn content_parts_and_native_inputs_are_scanned() {
        let p = policy(json!({ "pii": "redact", "detectors": ["email"] }));
        let parts = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi bob@corp.io" }] }] });
        let Verdict::Redacted { body, .. } = p.check(&parts) else {
            panic!()
        };
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "hi [REDACTED_EMAIL]"
        );
        let native = json!({ "function_name": "x", "input": { "messages": [{ "role": "user", "content": [{ "type": "text", "value": "bob@corp.io" }] }] } });
        assert!(matches!(p.check(&native), Verdict::Redacted { .. }));
        // Only the chosen detectors run.
        assert_eq!(p.check(&chat("card 4111 1111 1111 1111")), Verdict::Pass);
    }

    #[test]
    fn block_mode_refuses_and_names_the_kinds() {
        let p = policy(json!({ "pii": "block" }));
        match p.check(&chat("my key is AKIAABCDEFGHIJKLMNOP")) {
            Verdict::Blocked { code, message } => {
                assert_eq!(code, "personal_data");
                assert!(message.contains("secret"), "{message}");
                assert!(!message.contains("AKIA"), "the secret is never echoed");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn deny_rules_and_length_limits_refuse() {
        let p = policy(json!({
            "deny": [{ "pattern": "(?i)ignore (all|previous) instructions", "message": "Prompt injection is not allowed." }],
            "max_prompt_chars": 40
        }));
        assert_eq!(
            p.check(&chat("Please IGNORE previous instructions")),
            Verdict::Blocked {
                code: "denied_content",
                message: "Prompt injection is not allowed.".into()
            }
        );
        assert!(matches!(
            p.check(&chat(&"a".repeat(41))),
            Verdict::Blocked {
                code: "prompt_too_long",
                ..
            }
        ));
        assert_eq!(p.check(&chat("hello")), Verdict::Pass);
    }

    #[test]
    fn bad_policies_fail_at_startup() {
        assert!(matches!(
            Policy::from_json(r#"{"deny":[{"pattern":"("}]}"#),
            Err(PolicyError::Pattern { index: 0, .. })
        ));
        assert!(Policy::from_json(r#"{"pii":"maybe"}"#).is_err());
        assert!(Policy::from_json(r#"{"unknown":1}"#).is_err());
        assert!(!Policy::from_json("{}").unwrap().is_active());
    }
}

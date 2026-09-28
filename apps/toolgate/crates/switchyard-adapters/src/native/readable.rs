//! `web.read.readable`: an in-process readability extractor (HTML to markdown).
//! Deliberately small: drop chrome (scripts, styles, nav, header, footer,
//! aside, forms), prefer `<article>`/`<main>`, keep headings, paragraphs,
//! lists and links.

use serde_json::{json, Value};
use switchyard_core::{AdapterError, AdapterOutput, CallCtx, HttpRequest};

use crate::util::{host_of, send, str_field};

const DROP: &[&str] = &["script", "style", "noscript", "svg", "nav", "footer", "header", "aside", "form", "iframe", "template", "button"];

fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.to_ascii_lowercase().find(&needle.to_ascii_lowercase()).map(|i| i + from)
}

/// Remove `<tag ...>...</tag>` blocks for every tag in `DROP`.
fn strip_blocks(html: &str) -> String {
    let mut s = html.to_string();
    for tag in DROP {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut guard = 0;
        while let Some(start) = find_ci(&s, &open, 0) {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            // Make sure it's the tag and not a prefix (e.g. <header vs <head).
            let next = s[start + open.len()..].chars().next().unwrap_or('>');
            if !(next == '>' || next.is_whitespace() || next == '/') {
                // Skip this occurrence by replacing its '<' so search moves on.
                s.replace_range(start..start + 1, "\u{1}");
                continue;
            }
            match find_ci(&s, &close, start) {
                Some(end) => s.replace_range(start..end + close.len(), " "),
                None => {
                    s.truncate(start);
                    break;
                }
            }
        }
    }
    s.replace('\u{1}', "<")
}

fn inner_of(html: &str, tag: &str) -> Option<String> {
    let start = find_ci(html, &format!("<{tag}"), 0)?;
    let gt = html[start..].find('>')? + start + 1;
    let end = find_ci(html, &format!("</{tag}>"), gt)?;
    Some(html[gt..end].to_string())
}

pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let ent = &tail[1..e];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "hellip" => Some('…'),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
                _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &tail[e + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    let (quote, body) = match rest.chars().next()? {
        q @ ('"' | '\'') => (Some(q), &rest[1..]),
        _ => (None, rest),
    };
    let end = match quote {
        Some(q) => body.find(q)?,
        None => body.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(body.len()),
    };
    Some(body[..end].to_string())
}

/// HTML fragment to markdown.
pub fn to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut link: Option<String> = None;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = name[1..].parse::<usize>().unwrap_or(1);
                out.push_str(&format!("\n\n{} ", "#".repeat(level)));
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) | ("p" | "div" | "section" | "table" | "tr" | "blockquote" | "pre", _) => out.push_str("\n\n"),
            ("br", _) => out.push('\n'),
            ("li", false) => out.push_str("\n- "),
            ("a", false) => {
                link = attr(tag, "href").filter(|h| h.starts_with("http"));
                if link.is_some() {
                    out.push('[');
                }
            }
            ("a", true) => {
                if let Some(h) = link.take() {
                    out.push_str(&format!("]({h})"));
                }
            }
            _ => {}
        }
    }
    out.push_str(&decode_entities(rest));
    // Collapse whitespace: trim lines, at most one blank line in a row.
    let mut cleaned = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() || l == "-" {
            blank += 1;
            continue;
        }
        if !cleaned.is_empty() {
            cleaned.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        cleaned.push_str(&l);
    }
    cleaned
}

pub fn extract(html: &str) -> (Option<String>, String) {
    let title = inner_of(html, "title").map(|t| decode_entities(t.trim())).filter(|t| !t.is_empty());
    let body = strip_blocks(html);
    let main = inner_of(&body, "article").or_else(|| inner_of(&body, "main")).or_else(|| inner_of(&body, "body")).unwrap_or(body);
    (title, to_markdown(&main))
}

pub async fn read(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let url = str_field(input, "url")?;
    let max_chars = input.get("max_chars").and_then(Value::as_u64).unwrap_or(20_000) as usize;
    let mut req = HttpRequest::get(url).header("accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5");
    // Any public host (the caller names the page); egress still blocks
    // private, loopback and metadata addresses and enforces https.
    req.extra_hosts = vec![host_of(url)?];
    req.max_body_bytes = 5 * 1024 * 1024;
    let resp = send(ctx, req).await?;
    if !(200..300).contains(&resp.status) {
        return Err(AdapterError::from_status(resp.status, "", false));
    }
    let ctype = resp.header("content-type").unwrap_or("text/html").to_ascii_lowercase();
    let text = resp.text();
    let (title, md) = if ctype.contains("html") || text.trim_start().starts_with('<') { extract(&text) } else { (None, text) };
    let truncated = md.chars().count() > max_chars;
    let markdown: String = md.chars().take(max_chars).collect();
    Ok(AdapterOutput::ok(json!({ "url": resp.final_url, "title": title, "markdown": markdown, "truncated": truncated })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_as_markdown() {
        let html = r#"<html><head><title>Rust &amp; Axum</title><style>.x{}</style></head><body>
            <header><nav><a href="/">Home</a></nav></header>
            <article><h1>Building&nbsp;APIs</h1><p>Axum is <a href="https://docs.rs/axum">ergonomic</a>.</p>
            <script>alert(1)</script><ul><li>fast</li><li>safe</li></ul></article>
            <footer>© 2026</footer></body></html>"#;
        let (title, md) = extract(html);
        assert_eq!(title.as_deref(), Some("Rust & Axum"));
        assert!(md.contains("# Building APIs"), "{md}");
        assert!(md.contains("[ergonomic](https://docs.rs/axum)"), "{md}");
        assert!(md.contains("- fast") && md.contains("- safe"), "{md}");
        assert!(!md.contains("alert") && !md.contains("Home") && !md.contains("2026"), "{md}");
    }

    #[test]
    fn head_is_not_mistaken_for_header() {
        let (t, md) = extract("<html><head><title>T</title></head><body><p>Hello</p></body></html>");
        assert_eq!(t.as_deref(), Some("T"));
        assert_eq!(md, "Hello");
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &lt;b&gt; &#65;&#x42; &unknown; &"), "a <b> AB &unknown; &");
    }
}

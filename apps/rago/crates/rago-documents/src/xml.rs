//! Minimal, dependency-free XML builder + a well-formedness checker.
//!
//! The e-invoicing serializers ([`crate::ubl`], [`crate::cii`]) target the real
//! OASIS UBL 2.1 and UN/CEFACT CII schemas, which are namespace-qualified and
//! deeply nested. Rather than concatenate strings by hand (easy to leave a tag
//! unbalanced or a value unescaped), everything goes through [`Xml`], whose
//! closure-based [`Xml::element`] guarantees every open tag is matched by its
//! close tag, and whose [`Xml::leaf`]/attribute writers escape all text.
//!
//! [`is_well_formed`] re-parses the produced document (balanced tags, a single
//! root, quoted attributes, no raw `<`/`&`) so tests can assert the output
//! parses back — the dependency-free stand-in for full XSD validation.

/// Escape text content for an element body (`&`, `<`, `>`).
fn text_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape an attribute value (adds `"` on top of the text escapes).
fn attr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// A streaming XML document builder with automatic indentation.
pub struct Xml {
    buf: String,
    depth: usize,
}

impl Default for Xml {
    fn default() -> Self {
        Self::new()
    }
}

impl Xml {
    /// Start a new document with the standard XML declaration.
    pub fn new() -> Self {
        let mut buf = String::new();
        buf.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        Self { buf, depth: 0 }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.buf.push_str("  ");
        }
    }

    fn write_attrs(&mut self, attrs: &[(&str, &str)]) {
        for (k, v) in attrs {
            self.buf.push(' ');
            self.buf.push_str(k);
            self.buf.push_str("=\"");
            self.buf.push_str(&attr_escape(v));
            self.buf.push('"');
        }
    }

    /// Open an element (increases indentation depth). Prefer [`Xml::element`].
    pub fn open(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.indent();
        self.buf.push('<');
        self.buf.push_str(name);
        self.write_attrs(attrs);
        self.buf.push_str(">\n");
        self.depth += 1;
    }

    /// Close an element previously opened with [`Xml::open`].
    pub fn close(&mut self, name: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.indent();
        self.buf.push_str("</");
        self.buf.push_str(name);
        self.buf.push_str(">\n");
    }

    /// Write a nested element whose body is produced by `f`. The close tag is
    /// emitted automatically, so open/close can never drift apart.
    pub fn element(&mut self, name: &str, attrs: &[(&str, &str)], f: impl FnOnce(&mut Self)) {
        self.open(name, attrs);
        f(self);
        self.close(name);
    }

    /// Write a leaf element `<name attrs>text</name>` on one line.
    pub fn leaf(&mut self, name: &str, attrs: &[(&str, &str)], text: &str) {
        self.indent();
        self.buf.push('<');
        self.buf.push_str(name);
        self.write_attrs(attrs);
        self.buf.push('>');
        self.buf.push_str(&text_escape(text));
        self.buf.push_str("</");
        self.buf.push_str(name);
        self.buf.push_str(">\n");
    }

    /// Convenience: a leaf with no attributes.
    pub fn text(&mut self, name: &str, text: &str) {
        self.leaf(name, &[], text);
    }

    /// Write an XML comment (used to mirror Lago's structural annotations).
    pub fn comment(&mut self, text: &str) {
        self.indent();
        self.buf.push_str("<!-- ");
        // Comments cannot contain `--`; collapse defensively.
        self.buf.push_str(&text.replace("--", "—"));
        self.buf.push_str(" -->\n");
    }

    /// Finish the document and return the XML string.
    pub fn finish(self) -> String {
        self.buf
    }
}

/// Verify a document is well-formed: balanced/properly-nested tags, exactly one
/// root element, quoted attributes, and no raw `<` in text. Returns `Err` with
/// a human-readable reason otherwise. This is a structural stand-in for XSD
/// validation, not a full XML 1.0 parser.
pub fn is_well_formed(input: &str) -> Result<(), String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    let mut stack: Vec<String> = Vec::new();
    let mut roots_closed = 0usize;
    let mut saw_root = false;

    while i < n {
        let c = bytes[i];
        if c == b'<' {
            if input[i..].starts_with("<?") {
                // Processing instruction / declaration.
                let end = input[i..].find("?>").ok_or("unterminated <? ?>")?;
                i += end + 2;
                continue;
            }
            if input[i..].starts_with("<!--") {
                let end = input[i + 4..].find("-->").ok_or("unterminated comment")?;
                i += 4 + end + 3;
                continue;
            }
            if input[i..].starts_with("<!") {
                // DOCTYPE or similar — skip to '>'.
                let end = input[i..].find('>').ok_or("unterminated <!")?;
                i += end + 1;
                continue;
            }
            // A start or end tag. Find the closing '>'.
            let rel_end = input[i..].find('>').ok_or("unterminated tag")?;
            let tag = &input[i + 1..i + rel_end]; // between < and >
            i += rel_end + 1;

            if let Some(name) = tag.strip_prefix('/') {
                // End tag.
                let name = name.trim();
                let top = stack
                    .pop()
                    .ok_or_else(|| format!("unexpected end tag </{name}>"))?;
                if top != name {
                    return Err(format!("mismatched tags: <{top}> closed by </{name}>"));
                }
                if stack.is_empty() {
                    roots_closed += 1;
                }
            } else {
                // Start or self-closing tag. Validate attribute quoting.
                let (body, self_closing) = match tag.strip_suffix('/') {
                    Some(b) => (b, true),
                    None => (tag, false),
                };
                validate_attr_quotes(body)?;
                let name = body.split_whitespace().next().unwrap_or("").to_string();
                if name.is_empty() {
                    return Err("empty tag name".into());
                }
                if stack.is_empty() {
                    if saw_root && roots_closed >= 1 {
                        return Err("multiple root elements".into());
                    }
                    saw_root = true;
                }
                if !self_closing {
                    stack.push(name);
                } else if stack.is_empty() {
                    roots_closed += 1;
                }
            }
        } else if c == b'&' {
            // Text-level entity: must be a recognized/numeric entity ref.
            let rest = &input[i..];
            let semi = rest.find(';').ok_or("unterminated entity reference")?;
            let ent = &rest[1..semi];
            let ok = matches!(ent, "amp" | "lt" | "gt" | "quot" | "apos") || ent.starts_with('#');
            if !ok {
                return Err(format!("unknown entity reference &{ent};"));
            }
            i += semi + 1;
        } else {
            i += 1;
        }
    }

    if !stack.is_empty() {
        return Err(format!("unclosed element <{}>", stack.last().unwrap()));
    }
    if !saw_root {
        return Err("no root element".into());
    }
    if roots_closed != 1 {
        return Err(format!(
            "expected exactly one root element, closed {roots_closed}"
        ));
    }
    Ok(())
}

fn validate_attr_quotes(tag_body: &str) -> Result<(), String> {
    // Walk the attribute region; every '=' must be followed by a quoted value
    // with a matching closing quote.
    let bytes = tag_body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            // Skip spaces.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j >= bytes.len() || (bytes[j] != b'"' && bytes[j] != b'\'') {
                return Err("attribute value not quoted".into());
            }
            let quote = bytes[j];
            let mut k = j + 1;
            while k < bytes.len() && bytes[k] != quote {
                k += 1;
            }
            if k >= bytes.len() {
                return Err("unterminated attribute value".into());
            }
            i = k + 1;
        } else {
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_validates_nested() {
        let mut x = Xml::new();
        x.element("Root", &[("xmlns", "urn:test")], |x| {
            x.leaf("Id", &[], "A&B<C>");
            x.element("Child", &[("k", "v\"q")], |x| {
                x.text("Leaf", "hi");
            });
        });
        let out = x.finish();
        assert!(out.contains("A&amp;B&lt;C&gt;"));
        assert!(out.contains("k=\"v&quot;q\""));
        is_well_formed(&out).unwrap();
    }

    #[test]
    fn rejects_unbalanced() {
        assert!(is_well_formed("<?xml version=\"1.0\"?>\n<a><b></a>").is_err());
    }

    #[test]
    fn rejects_multiple_roots() {
        assert!(is_well_formed("<a></a><b></b>").is_err());
    }

    #[test]
    fn accepts_self_closing_root() {
        is_well_formed("<a/>").unwrap();
    }
}

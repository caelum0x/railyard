//! Pure-Rust syntax highlighting for blob viewing and unified-diff rendering.
//!
//! Highlighting uses [syntect](https://docs.rs/syntect) with its bundled
//! syntaxes and the pure-Rust `fancy-regex` engine (no C oniguruma), so it works
//! fully offline. Output is **class-based** (`ClassStyle::SpacedPrefixed`) rather
//! than inline styles: every token becomes `<span class="hl-…">`, and the page
//! ships one stylesheet from [`highlight_css`]. This keeps rendered HTML small,
//! themeable, and free of per-token style attributes.
//!
//! Everything degrades safely: files that are too large, look binary, or have no
//! matching syntax fall back to plain HTML-escaped text, and a highlighter error
//! on any line falls back to the escaped line. Highlighting therefore never
//! fails a page render.

use std::sync::OnceLock;

use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Namespaced class prefix so highlight classes never collide with site CSS.
const STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// Files larger than this are shown un-highlighted (escaped only). Highlighting
/// a multi-megabyte blob is neither useful nor cheap.
pub const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    // `newlines` variant is required by `parse_html_for_line_which_includes_newline`.
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The CSS for highlighted output. Combines syntect's class stylesheet (derived
/// from the bundled *InspiredGitHub* light theme) with diff line-background rules
/// used by [`highlight_diff_html`]. Cached after first construction.
pub fn highlight_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let themes = ThemeSet::load_defaults();
        let base = themes
            .themes
            .get("InspiredGitHub")
            .and_then(|t| css_for_theme_with_class_style(t, STYLE).ok())
            .unwrap_or_default();
        // Our own structural rules for the diff view (line markers + backgrounds).
        let diff = "\
.hlcode{tab-size:4}\
.hldiff .ctx{display:block}\
.hldiff .add{display:block;background:#e6ffec}\
.hldiff .del{display:block;background:#ffebe9}\
.hldiff .hunk{display:block;color:#8250df;background:#f6f8fa}\
.hldiff .marker{color:#6e7781;user-select:none}";
        format!("{base}\n{diff}")
    })
}

/// Escape the five HTML-significant characters. Used for fallback paths where
/// syntect is not applied (syntect escapes its own output internally).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// True when the bytes look binary (contain a NUL) — never highlight those.
fn looks_binary(content: &str) -> bool {
    content.as_bytes().contains(&0)
}

/// Pick a syntax by file extension, then by first line, else plain text.
fn syntax_for<'a>(ss: &'a SyntaxSet, filename: &str, content: &str) -> &'a SyntaxReference {
    let ext = filename.rsplit('.').next().unwrap_or("");
    if let Some(s) = ss.find_syntax_by_extension(ext) {
        return s;
    }
    // Fall back to the whole file name (e.g. `Makefile`, `Dockerfile`).
    if let Some(s) = ss.find_syntax_by_extension(filename) {
        return s;
    }
    if let Some(first) = content.lines().next() {
        if let Some(s) = ss.find_syntax_by_first_line(first) {
            return s;
        }
    }
    ss.find_syntax_plain_text()
}

/// Highlighted blob body: the inner HTML (class spans) to place inside
/// `<pre class="hlcode">…</pre>`.
///
/// Returns `None` when the file should be shown as plain escaped text (binary,
/// too large, or no useful syntax). The caller decides how to render the
/// fallback so this crate stays free of page-layout concerns.
pub fn highlight_blob_html(content: &str, filename: &str) -> Option<String> {
    if content.len() > MAX_HIGHLIGHT_BYTES || looks_binary(content) {
        return None;
    }
    let ss = syntax_set();
    let syntax = syntax_for(ss, filename, content);
    // Plain-text "syntax" adds nothing over an escape — signal fallback.
    if syntax.name.eq_ignore_ascii_case("Plain Text") {
        return None;
    }
    let mut gen = ClassedHTMLGenerator::new_with_class_style(syntax, ss, STYLE);
    for line in LinesWithEndings::from(content) {
        if gen.parse_html_for_line_which_includes_newline(line).is_err() {
            // Give up on highlighting; the caller falls back to escaped text.
            return None;
        }
    }
    Some(gen.finalize())
}

/// Render a single file's unified-diff `patch` as highlighted HTML.
///
/// Each line is wrapped in a `<span>` whose class encodes its diff role
/// (`add` / `del` / `ctx` / `hunk`); the leading `+`/`-`/space marker is kept in
/// a muted `marker` span and the remaining code is syntax-highlighted with the
/// file's language (best-effort, per line). Wrap the result in
/// `<pre class="hlcode hldiff">…</pre>`.
pub fn highlight_diff_html(file_path: &str, patch: &str) -> String {
    let ss = syntax_set();
    // Sample the code (strip markers) so first-line syntax detection can work.
    let syntax = syntax_for(ss, file_path, patch);
    let highlightable = !syntax.name.eq_ignore_ascii_case("Plain Text");

    let mut out = String::with_capacity(patch.len() * 2);
    for line in patch.lines() {
        let (class, marker, code) = classify(line);
        let code_html = if highlightable && !code.is_empty() {
            highlight_line(ss, syntax, code)
        } else {
            esc(code)
        };
        out.push_str(&format!(
            "<span class=\"{class}\"><span class=\"marker\">{}</span>{code_html}</span>",
            esc(marker)
        ));
    }
    out
}

/// Classify one patch line into (css-class, marker, code-after-marker).
fn classify(line: &str) -> (&'static str, &str, &str) {
    if line.starts_with("@@") {
        ("hunk", "", line)
    } else if line.starts_with("+++") || line.starts_with("---") {
        // File headers: treat as context (not add/del coloured).
        ("ctx", "", line)
    } else if let Some(rest) = line.strip_prefix('+') {
        ("add", "+", rest)
    } else if let Some(rest) = line.strip_prefix('-') {
        ("del", "-", rest)
    } else if let Some(rest) = line.strip_prefix(' ') {
        ("ctx", " ", rest)
    } else {
        ("ctx", "", line)
    }
}

/// Highlight a single line of code (no diff marker) to class-span HTML, escaping
/// on any highlighter error.
fn highlight_line(ss: &SyntaxSet, syntax: &SyntaxReference, code: &str) -> String {
    let mut gen = ClassedHTMLGenerator::new_with_class_style(syntax, ss, STYLE);
    let with_nl = format!("{code}\n");
    if gen
        .parse_html_for_line_which_includes_newline(&with_nl)
        .is_err()
    {
        return esc(code);
    }
    // Drop the trailing newline the generator preserved from our input.
    gen.finalize().trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_source_into_class_spans() {
        let html = highlight_blob_html("fn main() {}\n", "main.rs").expect("some");
        assert!(html.contains("hl-"), "expected prefixed classes: {html}");
        assert!(html.contains("main"));
    }

    #[test]
    fn binary_content_is_not_highlighted() {
        assert!(highlight_blob_html("abc\0def", "x.rs").is_none());
    }

    #[test]
    fn oversized_content_is_not_highlighted() {
        let big = "a".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert!(highlight_blob_html(&big, "x.rs").is_none());
    }

    #[test]
    fn plain_text_falls_back() {
        assert!(highlight_blob_html("just words\n", "notes.unknownext").is_none());
    }

    #[test]
    fn diff_marks_added_and_removed_lines() {
        let patch = "@@ -1,2 +1,2 @@\n fn a() {}\n-let x = 1;\n+let x = 2;\n";
        let html = highlight_diff_html("main.rs", patch);
        assert!(html.contains("class=\"add\""));
        assert!(html.contains("class=\"del\""));
        assert!(html.contains("class=\"hunk\""));
        // Markers are preserved in their own span.
        assert!(html.contains(">+</span>"));
        assert!(html.contains(">-</span>"));
    }

    #[test]
    fn css_contains_prefixed_rules_and_diff_rules() {
        let css = highlight_css();
        assert!(css.contains(".hl-"));
        assert!(css.contains(".hldiff .add"));
    }

    #[test]
    fn diff_escapes_html_metacharacters() {
        let patch = "+<script>alert(1)</script>\n";
        let html = highlight_diff_html("notes.unknownext", patch);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}

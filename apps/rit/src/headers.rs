//! Shared header-block parsing for commit and tag objects.
//!
//! Both commit and tag payloads share a layout: a block of `key value` header
//! lines, a single blank line, then a free-form message. A header value may
//! span multiple lines — continuation lines begin with a single space (this is
//! how Git stores multi-line `gpgsig` values). We preserve the exact value
//! bytes (with embedded newlines) so re-serialization is byte-identical.

use crate::error::{ForgeError, Result};

/// One header line: an ASCII key and its raw value bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The header keyword (e.g. `tree`, `parent`, `author`, `gpgsig`).
    pub key: String,
    /// The value bytes; multi-line values keep embedded `\n` separators.
    pub value: Vec<u8>,
}

/// The result of splitting a commit/tag payload.
pub struct HeaderBlock {
    /// Ordered header lines.
    pub headers: Vec<Header>,
    /// The message body (everything after the blank separator line).
    pub message: Vec<u8>,
}

/// Split a payload into ordered headers and the message body.
pub fn parse(payload: &[u8], kind: &'static str) -> Result<HeaderBlock> {
    // Header block ends at the first blank line ("\n\n").
    let (header_section, message) = match find_double_newline(payload) {
        Some(idx) => (&payload[..idx], payload[idx + 2..].to_vec()),
        // Some objects (a header-only tag) may have no message; then the whole
        // payload up to a trailing newline is headers.
        None => (payload, Vec::new()),
    };

    let mut headers: Vec<Header> = Vec::new();
    for line in split_lines(header_section) {
        if line.first() == Some(&b' ') {
            // Continuation of the previous header value.
            let last = headers.last_mut().ok_or_else(|| {
                ForgeError::malformed(kind, "continuation line with no preceding header")
            })?;
            last.value.push(b'\n');
            last.value.extend_from_slice(&line[1..]);
        } else {
            let space = line.iter().position(|&b| b == b' ').ok_or_else(|| {
                ForgeError::malformed(kind, "header line missing space separator")
            })?;
            let key = std::str::from_utf8(&line[..space])
                .map_err(|_| ForgeError::malformed(kind, "header key is not ASCII"))?
                .to_string();
            headers.push(Header {
                key,
                value: line[space + 1..].to_vec(),
            });
        }
    }

    Ok(HeaderBlock { headers, message })
}

/// Re-emit a header value, restoring the leading space on continuation lines.
pub fn serialize_value(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    for (i, &b) in value.iter().enumerate() {
        out.push(b);
        if b == b'\n' && i + 1 < value.len() {
            out.push(b' ');
        }
    }
    out
}

/// Locate the first `\n\n` in a byte slice.
fn find_double_newline(data: &[u8]) -> Option<usize> {
    data.windows(2).position(|w| w == b"\n\n")
}

/// Split a header section into lines, dropping the line terminators.
fn split_lines(section: &[u8]) -> Vec<&[u8]> {
    if section.is_empty() {
        return Vec::new();
    }
    section.split(|&b| b == b'\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_headers_and_message() {
        let payload = b"tree abc\nparent def\n\nhello world\n";
        let block = parse(payload, "commit").unwrap();
        assert_eq!(block.headers.len(), 2);
        assert_eq!(block.headers[0].key, "tree");
        assert_eq!(block.headers[0].value, b"abc");
        assert_eq!(block.message, b"hello world\n");
    }

    #[test]
    fn parses_continuation_lines() {
        let payload = b"gpgsig line1\n line2\n line3\n\nmsg\n";
        let block = parse(payload, "commit").unwrap();
        assert_eq!(block.headers.len(), 1);
        assert_eq!(block.headers[0].key, "gpgsig");
        assert_eq!(block.headers[0].value, b"line1\nline2\nline3");
        // round-trips back to the on-disk continuation form
        assert_eq!(serialize_value(&block.headers[0].value), b"line1\n line2\n line3");
    }
}

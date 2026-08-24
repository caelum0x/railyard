//! Author / committer / tagger signatures.
//!
//! A Git signature line looks like:
//!
//! ```text
//! Alice Example <alice@example.com> 1700000000 +0100
//! ```
//!
//! We keep the exact `raw` value so re-serialization is byte-identical to what
//! Git wrote, while also exposing parsed `name` / `email` / `time` for the API.

use crate::error::{ForgeError, Result};

/// A parsed Git identity + timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// The exact bytes after the `author `/`committer `/`tagger ` prefix.
    /// Preserved verbatim so serialization round-trips Git output exactly.
    pub raw: String,
    /// Display name (everything before the ` <`).
    pub name: String,
    /// Email address (between `<` and `>`).
    pub email: String,
    /// Unix timestamp in seconds.
    pub time: i64,
    /// Timezone offset token, e.g. `+0100`.
    pub tz: String,
}

impl Signature {
    /// Parse a signature value (the text after the header keyword).
    ///
    /// Fails closed on a missing email or timestamp rather than guessing.
    pub fn parse(value: &str, kind: &'static str) -> Result<Self> {
        let lt = value
            .find('<')
            .ok_or_else(|| ForgeError::malformed(kind, "signature missing '<' in email"))?;
        let gt = value
            .find('>')
            .ok_or_else(|| ForgeError::malformed(kind, "signature missing '>' in email"))?;
        if gt < lt {
            return Err(ForgeError::malformed(kind, "signature '>' precedes '<'"));
        }

        let name = value[..lt].trim().to_string();
        let email = value[lt + 1..gt].to_string();
        let rest = value[gt + 1..].trim();

        // rest is "<unixtime> <tz>". Both are optional in some hand-written
        // objects, but Git always writes them, so we require them.
        let mut parts = rest.split_whitespace();
        let time = parts
            .next()
            .ok_or_else(|| ForgeError::malformed(kind, "signature missing timestamp"))?
            .parse::<i64>()
            .map_err(|_| ForgeError::malformed(kind, "signature timestamp is not an integer"))?;
        let tz = parts
            .next()
            .ok_or_else(|| ForgeError::malformed(kind, "signature missing timezone"))?
            .to_string();

        Ok(Self {
            raw: value.to_string(),
            name,
            email,
            time,
            tz,
        })
    }

    /// The exact on-disk value (round-trips Git output).
    pub fn to_raw(&self) -> &str {
        &self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_signature() {
        let sig = Signature::parse("Alice Example <alice@example.com> 1700000000 +0100", "commit")
            .unwrap();
        assert_eq!(sig.name, "Alice Example");
        assert_eq!(sig.email, "alice@example.com");
        assert_eq!(sig.time, 1700000000);
        assert_eq!(sig.tz, "+0100");
        // raw preserved for byte-exact re-serialization
        assert_eq!(sig.to_raw(), "Alice Example <alice@example.com> 1700000000 +0100");
    }

    #[test]
    fn rejects_missing_email() {
        assert!(Signature::parse("Alice 1700000000 +0100", "commit").is_err());
    }

    #[test]
    fn rejects_missing_timestamp() {
        assert!(Signature::parse("Alice <a@b.c>", "commit").is_err());
    }
}

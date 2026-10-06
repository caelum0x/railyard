//! Boundary validation for user-controlled names.
//!
//! Org, repo, and user names become **filesystem path components** (the bare
//! repo lives at `<root>/<org>/<repo>.git`) and appear in URLs, so an unvalidated
//! name is a path-traversal and tenancy hazard. We accept a conservative,
//! GitHub-like character set and reject anything that could escape a directory.

use crate::error::CoreError;

/// Maximum length of a name component.
const MAX_NAME_LEN: usize = 100;

/// Validate a repository/organization/user name.
///
/// Rules (fail-closed): non-empty, at most [`MAX_NAME_LEN`] bytes, only ASCII
/// alphanumerics plus `-`, `_`, and `.`; must not be `.`/`..`; must not start
/// with `.` (hidden) or `-` (option-like); must not end in `.git` or `.lock`.
/// This keeps a name safe as both a single path component and a URL segment.
pub fn validate_name(kind: &'static str, name: &str) -> Result<(), CoreError> {
    let invalid = |value: &str| CoreError::InvalidName {
        kind,
        value: value.to_string(),
    };

    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid(name));
    }
    if name == "." || name == ".." {
        return Err(invalid(name));
    }
    if name.starts_with('.') || name.starts_with('-') {
        return Err(invalid(name));
    }
    if name.ends_with(".git") || name.ends_with(".lock") || name.ends_with(".wiki") {
        return Err(invalid(name));
    }
    let charset_ok = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    if !charset_ok {
        return Err(invalid(name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_reasonable_names() {
        for name in ["hello", "my-repo", "my_repo", "repo.v2", "Org123"] {
            assert!(validate_name("repo", name).is_ok(), "rejected {name}");
        }
    }

    #[test]
    fn rejects_traversal_and_tricks() {
        for name in [
            "", "..", ".", "../etc", "a/b", ".hidden", "-flag", "repo.git", "x.lock", "a b",
        ] {
            assert!(validate_name("repo", name).is_err(), "accepted {name}");
        }
    }
}

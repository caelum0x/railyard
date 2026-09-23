//! Reflog: read and append `.git/logs/{HEAD,refs/…}`.
//!
//! Git on-disk format — one LF-terminated line per entry:
//!
//! ```text
//! <old-sha1> SP <new-sha1> SP <name> SP <LT>email<GT> SP <unix-ts> SP <tz> TAB <message>
//! ```
//!
//! When a ref is created for the first time the `<old-sha1>` field is the
//! all-zeros null OID.  HEAD and the branch ref receive separate (but usually
//! identical) log files under `.git/logs/`.

use crate::error::Result;
use std::io::Write;
use std::path::{Path, PathBuf};

/// All-zeros OID written as `<old-sha1>` when a ref is created (no predecessor).
pub const NULL_OID_HEX: &str = "0000000000000000000000000000000000000000";

/// Hard upper bound on entries returned by [`read_reflog`] (guards against
/// enormous log files produced by an adversarial or buggy writer).
const MAX_REFLOG_ENTRIES: usize = 100_000;

/// One parsed reflog entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogEntry {
    /// Previous OID (40-hex; null-OID string when the ref was created).
    pub old_oid: String,
    /// New OID (40-hex).
    pub new_oid: String,
    /// Actor display name (may contain spaces).
    pub name: String,
    /// Actor email address (without angle brackets).
    pub email: String,
    /// Unix epoch timestamp.
    pub timestamp: i64,
    /// Timezone offset token, e.g. `+0000` or `-0700`.
    pub tz: String,
    /// Human-readable log message (the text that follows the TAB separator).
    pub message: String,
}

impl ReflogEntry {
    /// Serialize to a git-compatible reflog line **without** a trailing newline.
    pub fn format_line(&self) -> String {
        format!(
            "{} {} {} <{}> {} {}\t{}",
            self.old_oid,
            self.new_oid,
            self.name,
            self.email,
            self.timestamp,
            self.tz,
            self.message,
        )
    }

    /// Parse one line from a reflog file.
    ///
    /// Returns `None` for blank lines and `#`-prefixed comment lines (git
    /// never writes those, but they appear in test fixtures and documentation).
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        // TAB splits identity block from the free-text message.
        let (identity, message) = match line.split_once('\t') {
            Some((i, m)) => (i, m),
            None => (line, ""),
        };
        // First two whitespace tokens: old OID, new OID.
        let mut tokens = identity.splitn(3, ' ');
        let old_oid = tokens.next()?.to_string();
        let new_oid = tokens.next()?.to_string();
        let rest = tokens.next().unwrap_or("").trim();

        // `rest` = "Name With Spaces <email@host> <unix-ts> <tz>".
        // Locate `<email>` via angle brackets to separate name from timestamp.
        let lt = rest.find('<')?;
        // Search for `>` starting from just after `<`.
        let gt_rel = rest[lt + 1..].find('>')?;
        let gt = lt + 1 + gt_rel;

        let name = rest[..lt].trim().to_string();
        let email = rest[lt + 1..gt].to_string();
        let after = rest[gt + 1..].trim();

        let (ts_str, tz_str) = after.split_once(' ')?;
        let timestamp = ts_str.parse::<i64>().ok()?;
        let tz = tz_str.trim().to_string();

        Some(Self {
            old_oid,
            new_oid,
            name,
            email,
            timestamp,
            tz,
            message: message.to_string(),
        })
    }
}

// ---- path helpers -----------------------------------------------------------

/// Return `.git/logs/<refname>` for the given fully-qualified ref name.
///
/// For `HEAD` pass `"HEAD"` literally; for a branch pass `"refs/heads/main"`.
pub fn log_path_for_ref(git_dir: &Path, refname: &str) -> PathBuf {
    git_dir.join("logs").join(refname)
}

// ---- writer -----------------------------------------------------------------

/// Append `entry` to `log_path`, creating the file and its parent directories
/// when absent.
///
/// The write is fsynced: a crash immediately after this call leaves a complete,
/// correctly-terminated trailing line (no partial writes silently truncating the
/// log).
pub fn append_reflog_file(log_path: &Path, entry: &ReflogEntry) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", entry.format_line())?;
    file.sync_all()?;
    Ok(())
}

// ---- reader -----------------------------------------------------------------

/// Read up to `limit` reflog entries for `refname`, **newest first**.
///
/// The log file is stored oldest-first (append-only); this function reverses
/// the order before returning so callers see the most-recent entries first,
/// matching `git reflog show` output.
///
/// An absent log file is not an error — it is normal for a ref that has never
/// been logged, so an empty vec is returned.
///
/// `limit` is capped at [`MAX_REFLOG_ENTRIES`] regardless of the caller value.
pub fn read_reflog(git_dir: &Path, refname: &str, limit: usize) -> Result<Vec<ReflogEntry>> {
    let path = log_path_for_ref(git_dir, refname);
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let cap = limit.min(MAX_REFLOG_ENTRIES);
    let entries: Vec<ReflogEntry> = raw
        .lines()
        .rev()
        .filter_map(ReflogEntry::parse)
        .take(cap)
        .collect();
    Ok(entries)
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parse_roundtrip() {
        let entry = ReflogEntry {
            old_oid: NULL_OID_HEX.to_string(),
            new_oid: "abc123def0abc123def0abc123def0abc123def0".to_string(),
            name: "Alice Example".to_string(),
            email: "alice@example.com".to_string(),
            timestamp: 1_700_000_000,
            tz: "+0000".to_string(),
            message: "commit (initial): init".to_string(),
        };
        let line = entry.format_line();
        let parsed = ReflogEntry::parse(&line).expect("should parse a well-formed line");
        assert_eq!(parsed.old_oid, entry.old_oid);
        assert_eq!(parsed.new_oid, entry.new_oid);
        assert_eq!(parsed.name, entry.name);
        assert_eq!(parsed.email, entry.email);
        assert_eq!(parsed.timestamp, entry.timestamp);
        assert_eq!(parsed.tz, entry.tz);
        assert_eq!(parsed.message, entry.message);
    }

    #[test]
    fn name_with_spaces_parses() {
        let line =
            "0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 \
            Joe Q Public <joe@example.com> 1700000001 +0100\tbranch: Created from HEAD";
        let e = ReflogEntry::parse(line).unwrap();
        assert_eq!(e.name, "Joe Q Public");
        assert_eq!(e.email, "joe@example.com");
        assert_eq!(e.tz, "+0100");
        assert_eq!(e.message, "branch: Created from HEAD");
    }

    #[test]
    fn parse_skips_blank_and_comment_lines() {
        assert!(ReflogEntry::parse("").is_none());
        assert!(ReflogEntry::parse("   \n").is_none());
        assert!(ReflogEntry::parse("# comment").is_none());
    }

    #[test]
    fn append_and_read_reflog_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();

        let first = ReflogEntry {
            old_oid: NULL_OID_HEX.to_string(),
            new_oid: "1111111111111111111111111111111111111111".to_string(),
            name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
            timestamp: 1_700_000_000,
            tz: "+0000".to_string(),
            message: "commit (initial): first".to_string(),
        };
        let second = ReflogEntry {
            old_oid: "1111111111111111111111111111111111111111".to_string(),
            new_oid: "2222222222222222222222222222222222222222".to_string(),
            name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
            timestamp: 1_700_000_001,
            tz: "+0000".to_string(),
            message: "commit: second".to_string(),
        };

        let path = log_path_for_ref(git_dir, "refs/heads/main");
        append_reflog_file(&path, &first).unwrap();
        append_reflog_file(&path, &second).unwrap();

        // Read unbounded — should return newest first.
        let entries = read_reflog(git_dir, "refs/heads/main", 100).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].new_oid,
            "2222222222222222222222222222222222222222"
        );
        assert_eq!(
            entries[1].new_oid,
            "1111111111111111111111111111111111111111"
        );
    }

    #[test]
    fn read_reflog_absent_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let entries = read_reflog(dir.path(), "refs/heads/nonexistent", 10).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_reflog_limit_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        let path = log_path_for_ref(git_dir, "refs/heads/main");

        for i in 0..5u32 {
            let prev = format!("{:040x}", i);
            let next = format!("{:040x}", i + 1);
            let e = ReflogEntry {
                old_oid: prev,
                new_oid: next,
                name: "A".to_string(),
                email: "a@b.c".to_string(),
                timestamp: i as i64,
                tz: "+0000".to_string(),
                message: format!("commit: {i}"),
            };
            append_reflog_file(&path, &e).unwrap();
        }

        let limited = read_reflog(git_dir, "refs/heads/main", 3).unwrap();
        assert_eq!(limited.len(), 3);
        // Newest first — entry for i=4 should be first.
        assert_eq!(limited[0].timestamp, 4);
    }
}

//! Shared reporting types: what changed, and a human-readable tamper log.

use std::fmt;

/// The kind of integrity change detected during a re-scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// File exists in both manifest and disk, but the hash differs.
    Modified,
    /// File is on disk but not in the trusted manifest.
    Added,
    /// File is in the trusted manifest but missing from disk.
    Removed,
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChangeKind::Modified => "MODIFIED",
            ChangeKind::Added => "ADDED",
            ChangeKind::Removed => "REMOVED",
        };
        write!(f, "{s}")
    }
}

/// A single integrity finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: String,
    pub detail: String,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {} — {}", self.kind, self.path, self.detail)
    }
}

/// Result of comparing the current filesystem against a trusted manifest.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub files_checked: usize,
    pub changes: Vec<Change>,
}

impl ScanReport {
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty()
    }

    /// Render an immutable, ordered tamper log. Building a fresh String keeps
    /// this side-effect free (no mutation of shared state).
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Integrity scan: {} file(s) checked, {} change(s) detected\n",
            self.files_checked,
            self.changes.len()
        ));
        if self.is_clean() {
            out.push_str("  OK: all assets match the trusted manifest.\n");
        } else {
            for change in &self.changes {
                out.push_str(&format!("  {change}\n"));
            }
        }
        out
    }
}

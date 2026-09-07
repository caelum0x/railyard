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

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: ChangeKind, path: &str, detail: &str) -> Change {
        Change {
            kind,
            path: path.to_string(),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn change_kind_display() {
        assert_eq!(ChangeKind::Modified.to_string(), "MODIFIED");
        assert_eq!(ChangeKind::Added.to_string(), "ADDED");
        assert_eq!(ChangeKind::Removed.to_string(), "REMOVED");
    }

    #[test]
    fn change_display_format() {
        let c = change(ChangeKind::Modified, "config.cfg", "hash aaa… -> bbb…");
        assert_eq!(c.to_string(), "[MODIFIED] config.cfg — hash aaa… -> bbb…");
    }

    #[test]
    fn empty_report_is_clean() {
        let report = ScanReport::default();
        assert!(report.is_clean());
    }

    #[test]
    fn report_with_changes_is_not_clean() {
        let report = ScanReport {
            files_checked: 3,
            changes: vec![change(ChangeKind::Added, "cheat.dll", "unexpected file")],
        };
        assert!(!report.is_clean());
    }

    #[test]
    fn render_clean_report_reports_ok() {
        let report = ScanReport {
            files_checked: 5,
            changes: vec![],
        };
        let rendered = report.render();
        assert!(rendered.contains("5 file(s) checked, 0 change(s) detected"));
        assert!(rendered.contains("OK: all assets match the trusted manifest."));
    }

    #[test]
    fn render_dirty_report_lists_changes_in_order() {
        let report = ScanReport {
            files_checked: 2,
            changes: vec![
                change(ChangeKind::Added, "cheat.dll", "unexpected file"),
                change(ChangeKind::Modified, "config.cfg", "hash a… -> b…"),
                change(ChangeKind::Removed, "textures.pak", "missing on disk"),
            ],
        };
        let rendered = report.render();
        assert!(rendered.contains("2 file(s) checked, 3 change(s) detected"));
        assert!(!rendered.contains("OK: all assets match"));

        // Every change is rendered, and in the order supplied.
        let added_at = rendered.find("[ADDED] cheat.dll").expect("added line");
        let modified_at = rendered
            .find("[MODIFIED] config.cfg")
            .expect("modified line");
        let removed_at = rendered
            .find("[REMOVED] textures.pak")
            .expect("removed line");
        assert!(added_at < modified_at && modified_at < removed_at);
    }
}

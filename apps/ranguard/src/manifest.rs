//! Integrity manifest: fingerprint the tool's OWN sandbox asset files at a
//! "trusted" moment, sign the manifest with an HMAC, then later re-scan and
//! detect tampering (modified / added / removed) against that signed manifest.
//!
//! SCOPE: this only ever walks a directory the operator explicitly hands to the
//! tool (the project's own `assets/` sandbox). It does not touch system files.

use crate::hmac::{constant_time_eq, hmac_sha256};
use crate::report::{Change, ChangeKind, ScanReport};
use crate::sha256::{to_hex, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &str = "# ranguard-manifest v1";

/// A single trusted asset fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub rel_path: String,
    pub size: u64,
    pub sha256_hex: String,
}

/// A signed integrity manifest over a sandbox directory.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub created_at: u64,
    pub root: String,
    /// Sorted by rel_path for a canonical, deterministic representation.
    pub entries: Vec<Entry>,
    /// HMAC-SHA256 over the canonical body, hex-encoded.
    pub mac_hex: String,
}

/// Hash a file by streaming it in chunks (never loads huge files fully).
pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Recursively collect regular files under `root`, relative to `root`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    // Deterministic order regardless of filesystem enumeration order.
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "path not under root"))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
        // Symlinks and special files are intentionally skipped.
    }
    Ok(())
}

/// The exact bytes the MAC is computed over. Keeping this canonical (sorted,
/// fixed field order) is what makes verification reproducible.
fn canonical_body(created_at: u64, root: &str, entries: &[Entry]) -> String {
    let mut body = String::new();
    body.push_str(&format!("created_at\t{created_at}\n"));
    body.push_str(&format!("root\t{root}\n"));
    for e in entries {
        body.push_str(&format!("{}\t{}\t{}\n", e.rel_path, e.size, e.sha256_hex));
    }
    body
}

impl Manifest {
    /// Build a signed manifest over `root` at the current ("trusted") moment.
    pub fn build(root: &Path, signing_key: &[u8]) -> io::Result<Manifest> {
        let mut files = Vec::new();
        collect_files(root, root, &mut files)?;

        let mut entries = Vec::with_capacity(files.len());
        for (rel, path) in files {
            let meta = fs::metadata(&path)?;
            let digest = hash_file(&path)?;
            entries.push(Entry {
                rel_path: rel,
                size: meta.len(),
                sha256_hex: to_hex(&digest),
            });
        }
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let root_str = root.to_string_lossy().to_string();
        let body = canonical_body(created_at, &root_str, &entries);
        let mac = hmac_sha256(signing_key, body.as_bytes());

        Ok(Manifest {
            created_at,
            root: root_str,
            entries,
            mac_hex: to_hex(&mac),
        })
    }

    /// Serialize to a stable, line-based text format (no external deps).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(MAGIC);
        out.push('\n');
        out.push_str(&format!("mac\t{}\n", self.mac_hex));
        out.push_str("---\n");
        out.push_str(&canonical_body(self.created_at, &self.root, &self.entries));
        out
    }

    /// Parse a serialized manifest. Returns an error on malformed input.
    pub fn deserialize(text: &str) -> io::Result<Manifest> {
        let mut lines = text.lines();
        let magic = lines.next().unwrap_or_default();
        if magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad manifest magic header",
            ));
        }

        let mut mac_hex = String::new();
        for line in lines.by_ref() {
            if line == "---" {
                break;
            }
            if let Some(v) = line.strip_prefix("mac\t") {
                mac_hex = v.to_string();
            }
        }

        let mut created_at = 0u64;
        let mut root = String::new();
        let mut entries = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            match parts.as_slice() {
                ["created_at", v] => {
                    created_at = v.parse().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad created_at")
                    })?;
                }
                ["root", v] => root = v.to_string(),
                [rel, size, hex] => {
                    let size = size.parse().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad size field")
                    })?;
                    entries.push(Entry {
                        rel_path: rel.to_string(),
                        size,
                        sha256_hex: hex.to_string(),
                    });
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed manifest line",
                    ))
                }
            }
        }

        Ok(Manifest {
            created_at,
            root,
            entries,
            mac_hex,
        })
    }

    /// Verify that the manifest itself has not been tampered with, using the
    /// same signing key. This defends the manifest-on-disk from silent edits.
    pub fn verify_signature(&self, signing_key: &[u8]) -> bool {
        let body = canonical_body(self.created_at, &self.root, &self.entries);
        let expected = hmac_sha256(signing_key, body.as_bytes());
        let expected_hex = to_hex(&expected);
        constant_time_eq(expected_hex.as_bytes(), self.mac_hex.as_bytes())
    }

    /// Re-scan `root` now and diff it against this trusted manifest.
    pub fn scan(&self, root: &Path) -> io::Result<ScanReport> {
        let mut files = Vec::new();
        collect_files(root, root, &mut files)?;

        // Current state, keyed by relative path.
        let mut current: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (rel, path) in files {
            current.insert(rel, path);
        }

        let trusted: BTreeMap<&str, &Entry> =
            self.entries.iter().map(|e| (e.rel_path.as_str(), e)).collect();

        let mut changes = Vec::new();
        let mut checked = 0usize;

        // Modified / removed relative to the trusted set.
        for (rel, entry) in &trusted {
            match current.get(*rel) {
                Some(path) => {
                    checked += 1;
                    let digest = hash_file(path)?;
                    let hex = to_hex(&digest);
                    if hex != entry.sha256_hex {
                        changes.push(Change {
                            kind: ChangeKind::Modified,
                            path: (*rel).to_string(),
                            detail: format!(
                                "hash {}… -> {}…",
                                &entry.sha256_hex[..12.min(entry.sha256_hex.len())],
                                &hex[..12.min(hex.len())]
                            ),
                        });
                    }
                }
                None => changes.push(Change {
                    kind: ChangeKind::Removed,
                    path: (*rel).to_string(),
                    detail: "file present in manifest is missing on disk".to_string(),
                }),
            }
        }

        // Added files not present in the trusted set.
        for rel in current.keys() {
            if !trusted.contains_key(rel.as_str()) {
                changes.push(Change {
                    kind: ChangeKind::Added,
                    path: rel.clone(),
                    detail: "unexpected file not in trusted manifest".to_string(),
                });
            }
        }

        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(ScanReport {
            files_checked: checked,
            changes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("ranguard_test_{tag}_{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_file(dir: &Path, name: &str, contents: &[u8]) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(contents).unwrap();
    }

    #[test]
    fn clean_scan_has_no_changes() {
        let dir = temp_dir("clean");
        write_file(&dir, "a.txt", b"hello");
        write_file(&dir, "sub/b.txt", b"world");
        let m = Manifest::build(&dir, b"key").unwrap();
        let report = m.scan(&dir).unwrap();
        assert!(report.is_clean());
        assert_eq!(report.files_checked, 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_modified_added_removed() {
        let dir = temp_dir("tamper");
        write_file(&dir, "keep.txt", b"same");
        write_file(&dir, "change.txt", b"before");
        write_file(&dir, "gone.txt", b"delete me");
        let m = Manifest::build(&dir, b"key").unwrap();

        // Tamper: modify one, add one, remove one.
        write_file(&dir, "change.txt", b"AFTER-tampered");
        write_file(&dir, "new.txt", b"sneaky");
        fs::remove_file(dir.join("gone.txt")).unwrap();

        let report = m.scan(&dir).unwrap();
        assert_eq!(report.changes.len(), 3);
        let kinds: Vec<_> = report.changes.iter().map(|c| c.kind.clone()).collect();
        assert!(kinds.contains(&ChangeKind::Modified));
        assert!(kinds.contains(&ChangeKind::Added));
        assert!(kinds.contains(&ChangeKind::Removed));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_roundtrip_and_signature() {
        let dir = temp_dir("sig");
        write_file(&dir, "x.txt", b"data");
        let m = Manifest::build(&dir, b"secret").unwrap();
        let text = m.serialize();
        let parsed = Manifest::deserialize(&text).unwrap();
        assert!(parsed.verify_signature(b"secret"));
        assert!(!parsed.verify_signature(b"wrong-key"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tampered_manifest_fails_signature() {
        let dir = temp_dir("badsig");
        write_file(&dir, "x.txt", b"data");
        let m = Manifest::build(&dir, b"secret").unwrap();
        let mut parsed = Manifest::deserialize(&m.serialize()).unwrap();
        // Silently edit a recorded hash without re-signing.
        parsed.entries[0].sha256_hex = "0".repeat(64);
        assert!(!parsed.verify_signature(b"secret"));
        fs::remove_dir_all(&dir).ok();
    }
}

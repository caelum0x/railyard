//! References: `HEAD`, loose refs under `.git/refs/`, and `.git/packed-refs`.
//!
//! Supports symbolic refs (`ref: refs/heads/main`), direct oids, and the
//! standard shorthand resolution order Git uses (`main` ->
//! `refs/heads/main`, etc.). Enough to answer "what commit is HEAD?".

use crate::error::{ForgeError, Result};
use crate::oid::ObjectId;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A resolved-one-step reference value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefTarget {
    /// A symbolic ref pointing at another ref name.
    Symbolic(String),
    /// A direct object id.
    Direct(ObjectId),
}

/// Ref storage for one repository.
pub struct Refs {
    git_dir: PathBuf,
}

/// Guard against symbolic-ref cycles.
const MAX_REF_DEPTH: usize = 10;

impl Refs {
    /// Create ref access rooted at a `.git` directory.
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
        }
    }

    /// Read and interpret `.git/HEAD`.
    pub fn read_head(&self) -> Result<RefTarget> {
        let raw = std::fs::read_to_string(self.git_dir.join("HEAD"))
            .map_err(|_| ForgeError::InvalidRef("HEAD is unreadable".into()))?;
        parse_ref_content(raw.trim())
    }

    /// Resolve `HEAD` all the way to an object id.
    pub fn resolve_head(&self) -> Result<ObjectId> {
        match self.read_head()? {
            RefTarget::Direct(oid) => Ok(oid),
            RefTarget::Symbolic(name) => self.resolve(&name),
        }
    }

    /// Resolve a ref name (full or shorthand) to an object id.
    pub fn resolve(&self, name: &str) -> Result<ObjectId> {
        if name != "HEAD" && name.starts_with("refs/") {
            validate_refname(name)?;
        }
        self.resolve_depth(name, 0)
    }

    fn resolve_depth(&self, name: &str, depth: usize) -> Result<ObjectId> {
        if depth > MAX_REF_DEPTH {
            return Err(ForgeError::InvalidRef(format!(
                "symbolic ref chain too deep at {name}"
            )));
        }
        if name == "HEAD" {
            return self.resolve_head();
        }
        for candidate in self.candidate_paths(name) {
            if let Some(target) = self.read_loose_ref(&candidate)? {
                return match target {
                    RefTarget::Direct(oid) => Ok(oid),
                    RefTarget::Symbolic(next) => self.resolve_depth(&next, depth + 1),
                };
            }
        }
        // Fall back to packed-refs, keyed by full ref name.
        let packed = self.read_packed_refs()?;
        for candidate in self.candidate_names(name) {
            if let Some(oid) = packed.get(&candidate) {
                return Ok(*oid);
            }
        }
        Err(ForgeError::InvalidRef(name.to_string()))
    }

    /// List all loose ref names and their resolved oids (used by `log` and
    /// for listing). Packed refs are merged in, loose winning on conflict.
    pub fn list(&self) -> Result<Vec<(String, ObjectId)>> {
        let mut out: HashMap<String, ObjectId> = self.read_packed_refs()?;
        let refs_root = self.git_dir.join("refs");
        collect_loose(&refs_root, &refs_root, &mut out)?;
        let mut v: Vec<(String, ObjectId)> = out.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }

    /// Read one loose ref file, if present, one step (no recursion).
    fn read_loose_ref(&self, refname: &str) -> Result<Option<RefTarget>> {
        validate_refname(refname)?;
        let path = self.git_dir.join(refname);
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(parse_ref_content(raw.trim())?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically update a direct ref if it still has `expected` as its current
    /// value. `None` means the ref must not exist. A `.lock` file serializes
    /// writers; publishing is one fsync + rename + parent-directory fsync.
    ///
    /// Packed refs are supported: a successful update writes a loose ref that
    /// shadows the packed value, matching Git's storage behavior.
    pub fn compare_and_swap(
        &self,
        name: &str,
        expected: Option<ObjectId>,
        new: ObjectId,
    ) -> Result<()> {
        validate_refname(name)?;
        let path = self.git_dir.join(name);
        let parent = path
            .parent()
            .ok_or_else(|| ForgeError::UnsafePath(name.to_string()))?;
        std::fs::create_dir_all(parent)?;
        reject_symlink_components(&self.git_dir, parent)?;

        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let mut lock = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ForgeError::RefLocked(name.to_string()))
            }
            Err(error) => return Err(error.into()),
        };

        let result = (|| {
            let actual = match self.resolve(name) {
                Ok(oid) => Some(oid),
                Err(ForgeError::InvalidRef(_)) => None,
                Err(error) => return Err(error),
            };
            if actual != expected {
                return Err(ForgeError::RefConflict {
                    name: name.to_string(),
                    expected: expected
                        .map(|oid| oid.to_hex())
                        .unwrap_or_else(|| "<missing>".into()),
                    actual: actual
                        .map(|oid| oid.to_hex())
                        .unwrap_or_else(|| "<missing>".into()),
                });
            }

            writeln!(lock, "{new}")?;
            lock.sync_all()?;
            std::fs::rename(&lock_path, &path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&lock_path);
        }
        result
    }

    /// Atomically remove one loose ref if it still resolves to `expected`.
    ///
    /// The lock and compare step use the same protocol as
    /// [`compare_and_swap`](Self::compare_and_swap). A ref backed by
    /// `packed-refs` fails closed: removing a loose shadow could reveal an older
    /// packed value, and rewriting `packed-refs` belongs to a separate
    /// multi-storage transaction rather than this one-file primitive.
    pub fn compare_and_delete(&self, name: &str, expected: ObjectId) -> Result<()> {
        validate_refname(name)?;
        let path = self.git_dir.join(name);
        let parent = path
            .parent()
            .ok_or_else(|| ForgeError::UnsafePath(name.to_string()))?;
        std::fs::create_dir_all(parent)?;
        reject_symlink_components(&self.git_dir, parent)?;
        if std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ForgeError::UnsafePath(path.display().to_string()));
        }

        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let lock = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ForgeError::RefLocked(name.to_string()))
            }
            Err(error) => return Err(error.into()),
        };

        let result = (|| {
            let actual = match self.resolve(name) {
                Ok(oid) => Some(oid),
                Err(ForgeError::InvalidRef(_)) => None,
                Err(error) => return Err(error),
            };
            if actual != Some(expected) {
                return Err(ForgeError::RefConflict {
                    name: name.to_string(),
                    expected: expected.to_hex(),
                    actual: actual
                        .map(|oid| oid.to_hex())
                        .unwrap_or_else(|| "<missing>".into()),
                });
            }
            if self.read_packed_refs()?.contains_key(name) {
                return Err(ForgeError::ReceiveRejected(format!(
                    "cannot safely delete packed ref {name} with the loose-ref transaction"
                )));
            }
            std::fs::remove_file(&path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        drop(lock);
        let cleanup = std::fs::remove_file(&lock_path);
        if result.is_ok() {
            cleanup?;
        }
        result
    }

    /// Append one reflog entry to `.git/logs/<refname>` and, when this ref is
    /// HEAD's current symbolic target, also to `.git/logs/HEAD`.
    ///
    /// `old` is `None` when the ref is being created (the all-zeros null OID is
    /// written in that position).  Both log files are created with their parent
    /// directories on first use; the write is fsynced before returning.
    ///
    /// This is intentionally a low-level primitive — callers supply the
    /// identity and message so higher-level operations (commit, branch-create,
    /// branch-rename, …) can record the appropriate context.
    pub fn append_reflog(
        &self,
        refname: &str,
        old: Option<ObjectId>,
        new: ObjectId,
        name: &str,
        email: &str,
        timestamp: i64,
        tz: &str,
        message: &str,
    ) -> Result<()> {
        use crate::reflog::{append_reflog_file, log_path_for_ref, ReflogEntry, NULL_OID_HEX};
        let entry = ReflogEntry {
            old_oid: old
                .map(|oid| oid.to_hex())
                .unwrap_or_else(|| NULL_OID_HEX.to_string()),
            new_oid: new.to_hex(),
            name: name.to_string(),
            email: email.to_string(),
            timestamp,
            tz: tz.to_string(),
            message: message.to_string(),
        };
        // Always write to the ref's own log file.
        append_reflog_file(&log_path_for_ref(&self.git_dir, refname), &entry)?;
        // When HEAD is a symbolic ref pointing at this ref, also update logs/HEAD
        // so that `git reflog` (which reads logs/HEAD) sees the same history.
        if let Ok(RefTarget::Symbolic(head_target)) = self.read_head() {
            if head_target == refname {
                append_reflog_file(&log_path_for_ref(&self.git_dir, "HEAD"), &entry)?;
            }
        }
        Ok(())
    }

    /// Parse `.git/packed-refs` into a name -> oid map (peel lines ignored).
    fn read_packed_refs(&self) -> Result<HashMap<String, ObjectId>> {
        let path = self.git_dir.join("packed-refs");
        let mut map = HashMap::new();
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return Ok(map), // absent packed-refs is normal
        };
        for line in raw.lines() {
            if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty() {
                continue;
            }
            if let Some((oid_hex, name)) = line.split_once(' ') {
                if let Ok(oid) = ObjectId::from_hex(oid_hex) {
                    map.insert(name.trim().to_string(), oid);
                }
            }
        }
        Ok(map)
    }

    /// Candidate loose-ref *paths* (relative to `.git`) for a shorthand name.
    fn candidate_paths(&self, name: &str) -> Vec<String> {
        self.candidate_names(name)
    }

    /// Standard Git ref resolution order for a shorthand.
    fn candidate_names(&self, name: &str) -> Vec<String> {
        if name.starts_with("refs/") {
            return vec![name.to_string()];
        }
        vec![
            format!("refs/tags/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ]
    }
}

/// Interpret raw ref file / HEAD content.
fn parse_ref_content(content: &str) -> Result<RefTarget> {
    if let Some(target) = content.strip_prefix("ref:") {
        Ok(RefTarget::Symbolic(target.trim().to_string()))
    } else {
        Ok(RefTarget::Direct(ObjectId::from_hex(content)?))
    }
}

/// Conservative subset of `git check-ref-format`. Most importantly this keeps
/// repository-controlled names inside `.git/refs` and reserves `.lock` for the
/// transaction protocol.
pub fn validate_refname(name: &str) -> Result<()> {
    let invalid = !name.starts_with("refs/")
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name
            .bytes()
            .any(|b| b <= b' ' || b == 0x7f || b"~^:?*[\\".contains(&b))
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if invalid {
        return Err(ForgeError::InvalidRef(name.to_string()));
    }
    Ok(())
}

fn reject_symlink_components(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| ForgeError::UnsafePath(target.display().to_string()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ForgeError::UnsafePath(current.display().to_string()));
        }
    }
    Ok(())
}

/// Recursively collect loose refs under `dir`, keyed by full ref name.
fn collect_loose(root: &Path, dir: &Path, out: &mut HashMap<String, ObjectId>) -> Result<()> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if entry
            .file_type()
            .map(|kind| kind.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        if path.is_dir() {
            collect_loose(root, &path, out)?;
        } else if let Ok(raw) = std::fs::read_to_string(&path) {
            // Build "refs/..." name relative to the .git dir.
            if let Ok(rel) = path.strip_prefix(root.parent().unwrap_or(root)) {
                let name = rel.to_string_lossy().replace('\\', "/");
                if let Ok(RefTarget::Direct(oid)) = parse_ref_content(raw.trim()) {
                    out.insert(name, oid);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn parses_symbolic_head() {
        let t = parse_ref_content("ref: refs/heads/main").unwrap();
        assert_eq!(t, RefTarget::Symbolic("refs/heads/main".into()));
    }

    #[test]
    fn parses_detached_head() {
        let hex = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let t = parse_ref_content(hex).unwrap();
        assert_eq!(t, RefTarget::Direct(ObjectId::from_hex(hex).unwrap()));
    }

    #[test]
    fn rejects_ref_path_traversal() {
        for name in [
            "refs/heads/../../outside",
            "../outside",
            "refs/heads/x.lock",
        ] {
            assert!(validate_refname(name).is_err(), "accepted {name}");
        }
    }

    #[test]
    fn compare_and_swap_allows_only_one_concurrent_winner() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let old = ObjectId::from_hex("1111111111111111111111111111111111111111").unwrap();
        let left = ObjectId::from_hex("2222222222222222222222222222222222222222").unwrap();
        let right = ObjectId::from_hex("3333333333333333333333333333333333333333").unwrap();
        std::fs::write(dir.path().join("refs/heads/main"), format!("{old}\n")).unwrap();

        let root = Arc::new(dir.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(3));
        let spawn = |next| {
            let root = root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let refs = Refs::new(&root);
                barrier.wait();
                refs.compare_and_swap("refs/heads/main", Some(old), next)
            })
        };
        let a = spawn(left);
        let b = spawn(right);
        barrier.wait();
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let actual = Refs::new(&root).resolve("refs/heads/main").unwrap();
        assert!(actual == left || actual == right);
        assert!(!dir.path().join("refs/heads/main.lock").exists());
    }

    #[test]
    fn compare_and_delete_removes_only_the_expected_loose_ref() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let old = ObjectId::from_hex("1111111111111111111111111111111111111111").unwrap();
        let wrong = ObjectId::from_hex("2222222222222222222222222222222222222222").unwrap();
        std::fs::write(dir.path().join("refs/heads/main"), format!("{old}\n")).unwrap();
        let refs = Refs::new(dir.path());

        assert!(matches!(
            refs.compare_and_delete("refs/heads/main", wrong),
            Err(ForgeError::RefConflict { .. })
        ));
        assert_eq!(refs.resolve("refs/heads/main").unwrap(), old);
        refs.compare_and_delete("refs/heads/main", old).unwrap();
        assert!(refs.resolve("refs/heads/main").is_err());
        assert!(!dir.path().join("refs/heads/main.lock").exists());
    }

    #[test]
    fn compare_and_delete_refuses_a_packed_backing_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let old = ObjectId::from_hex("1111111111111111111111111111111111111111").unwrap();
        std::fs::write(
            dir.path().join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled\n{old} refs/heads/main\n"),
        )
        .unwrap();
        let refs = Refs::new(dir.path());
        assert!(matches!(
            refs.compare_and_delete("refs/heads/main", old),
            Err(ForgeError::ReceiveRejected(_))
        ));
        assert_eq!(refs.resolve("refs/heads/main").unwrap(), old);
        assert!(!dir.path().join("refs/heads/main.lock").exists());
    }
}

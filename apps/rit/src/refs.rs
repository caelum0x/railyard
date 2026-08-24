//! References: `HEAD`, loose refs under `.git/refs/`, and `.git/packed-refs`.
//!
//! Supports symbolic refs (`ref: refs/heads/main`), direct oids, and the
//! standard shorthand resolution order Git uses (`main` ->
//! `refs/heads/main`, etc.). Enough to answer "what commit is HEAD?".

use crate::error::{ForgeError, Result};
use crate::oid::ObjectId;
use std::collections::HashMap;
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
        let path = self.git_dir.join(refname);
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(parse_ref_content(raw.trim())?)),
            Err(_) => Ok(None),
        }
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
            name.to_string(),
            format!("refs/{name}"),
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

/// Recursively collect loose refs under `dir`, keyed by full ref name.
fn collect_loose(
    root: &Path,
    dir: &Path,
    out: &mut HashMap<String, ObjectId>,
) -> Result<()> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
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
}

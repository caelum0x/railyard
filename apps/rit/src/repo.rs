//! The porcelain-ish repository API — the layer that proves the PLAN's first
//! promise: "inspect, create, verify ... standard Git repositories without
//! corrupting data."
//!
//! [`Repository`] ties the object database and refs together and exposes the
//! equivalents of `hash-object -w`, `cat-file -p/-t`, `rev-parse`, `ls-tree`,
//! and `log`.

use crate::blob::Blob;
use crate::commit::Commit;
use crate::error::{ForgeError, Result};
use crate::object::{Object, ObjectType};
use crate::odb::Odb;
use crate::oid::ObjectId;
use crate::refs::Refs;
use crate::tree::Tree;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// An opened Git repository (loose-object scope).
pub struct Repository {
    git_dir: PathBuf,
    odb: Odb,
    refs: Refs,
}

impl Repository {
    /// Open a repository given its `.git` directory directly.
    pub fn open_git_dir(git_dir: impl AsRef<Path>) -> Result<Self> {
        let git_dir = git_dir.as_ref().to_path_buf();
        if !git_dir.join("objects").is_dir() {
            return Err(ForgeError::NotARepository(git_dir.display().to_string()));
        }
        let odb = Odb::new(&git_dir);
        let refs = Refs::new(&git_dir);
        Ok(Self { git_dir, odb, refs })
    }

    /// Discover a repository by walking up from `start` looking for `.git`.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let start = start.as_ref();
        let mut cur: Option<&Path> = Some(start);
        while let Some(dir) = cur {
            let candidate = dir.join(".git");
            if candidate.is_dir() {
                return Self::open_git_dir(candidate);
            }
            // Also accept being handed a `.git` dir itself.
            if dir.file_name().map(|n| n == ".git").unwrap_or(false) && dir.join("objects").is_dir()
            {
                return Self::open_git_dir(dir);
            }
            cur = dir.parent();
        }
        Err(ForgeError::NotARepository(start.display().to_string()))
    }

    /// The `.git` directory path.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Borrow the object database.
    pub fn odb(&self) -> &Odb {
        &self.odb
    }

    /// Borrow ref access.
    pub fn refs(&self) -> &Refs {
        &self.refs
    }

    // --- object plumbing --------------------------------------------------

    /// `git hash-object [-w]`: hash `data` as `obj_type`, optionally writing it.
    pub fn hash_object(&self, obj_type: ObjectType, data: &[u8], write: bool) -> Result<ObjectId> {
        if write {
            self.odb.write(obj_type, data)
        } else {
            Ok(crate::object::hash_content(obj_type, data).0)
        }
    }

    /// `git cat-file <type> <oid>`: read and parse an object.
    pub fn read_object(&self, oid: &ObjectId) -> Result<Object> {
        self.odb.read(oid)
    }

    /// `git cat-file -t <oid>`: an object's type.
    pub fn object_type(&self, oid: &ObjectId) -> Result<ObjectType> {
        self.odb.object_type(oid)
    }

    /// Read an object expected to be a blob.
    pub fn blob(&self, oid: &ObjectId) -> Result<Blob> {
        match self.odb.read(oid)? {
            Object::Blob(b) => Ok(b),
            other => Err(type_error(oid, "blob", other)),
        }
    }

    /// Read an object expected to be a tree.
    pub fn tree(&self, oid: &ObjectId) -> Result<Tree> {
        match self.odb.read(oid)? {
            Object::Tree(t) => Ok(t),
            other => Err(type_error(oid, "tree", other)),
        }
    }

    /// Read an object expected to be a commit.
    pub fn commit(&self, oid: &ObjectId) -> Result<Commit> {
        match self.odb.read(oid)? {
            Object::Commit(c) => Ok(c),
            other => Err(type_error(oid, "commit", other)),
        }
    }

    /// Verify that a stored object is not corrupt (its bytes hash to its oid).
    /// `read_raw` performs the check; this exposes it as a boolean.
    pub fn verify(&self, oid: &ObjectId) -> Result<bool> {
        match self.odb.read_raw(oid) {
            Ok(_) => Ok(true),
            Err(ForgeError::HashMismatch { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    // --- refs / revisions -------------------------------------------------

    /// The commit `HEAD` points at.
    pub fn head(&self) -> Result<ObjectId> {
        self.refs.resolve_head()
    }

    /// Read the commit `HEAD` currently points at.
    pub fn head_commit(&self) -> Result<Commit> {
        let oid = self.head()?;
        self.peel_to_commit(oid)
    }

    /// Read the tree of the `HEAD` commit.
    pub fn head_tree(&self) -> Result<Tree> {
        let commit = self.head_commit()?;
        self.tree(&commit.tree)
    }

    /// Resolve a revision string: `HEAD`, a ref name, a full 40-hex oid, or an
    /// unambiguous abbreviated oid prefix.
    pub fn resolve(&self, rev: &str) -> Result<ObjectId> {
        let rev = rev.trim();
        // Full oid?
        if rev.len() == 40 {
            if let Ok(oid) = ObjectId::from_hex(rev) {
                return Ok(oid);
            }
        }
        // Ref (HEAD, branch, tag, remote)?
        if let Ok(oid) = self.refs.resolve(rev) {
            return Ok(oid);
        }
        // Abbreviated oid?
        if rev.len() >= 4 && rev.len() < 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
            return self.odb.resolve_prefix(rev);
        }
        Err(ForgeError::InvalidRef(rev.to_string()))
    }

    /// If `oid` is an annotated tag, follow it to the commit it references;
    /// otherwise require `oid` to already be a commit.
    pub fn peel_to_commit(&self, oid: ObjectId) -> Result<Commit> {
        let mut current = oid;
        for _ in 0..10 {
            match self.odb.read(&current)? {
                Object::Commit(c) => return Ok(c),
                Object::Tag(t) => current = t.object,
                other => return Err(type_error(&current, "commit", other)),
            }
        }
        Err(ForgeError::InvalidRef(format!(
            "{oid} does not peel to a commit"
        )))
    }

    /// `git log`: walk history from `start`, newest first by committer time.
    ///
    /// Bounded by a visited set so cycles or malicious graphs cannot loop
    /// forever (PLAN "Security contract": no unbounded graph walks).
    pub fn log(&self, start: ObjectId) -> Result<Vec<(ObjectId, Commit)>> {
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut stack: Vec<ObjectId> = vec![start];
        let mut out: Vec<(ObjectId, Commit)> = Vec::new();

        while let Some(oid) = stack.pop() {
            if !seen.insert(oid) {
                continue;
            }
            let commit = self.peel_to_commit(oid)?;
            for parent in &commit.parents {
                if !seen.contains(parent) {
                    stack.push(*parent);
                }
            }
            out.push((oid, commit));
        }

        // Newest first; committer time is a stable, Git-visible ordering.
        out.sort_by(|a, b| b.1.committer.time.cmp(&a.1.committer.time));
        Ok(out)
    }
}

/// Build a helpful "wrong type" error.
fn type_error(oid: &ObjectId, expected: &'static str, got: Object) -> ForgeError {
    ForgeError::malformed(
        "object",
        format!("{oid} is a {}, expected {expected}", got.object_type()),
    )
}

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
use crate::index::{validate_index_path, Index, IndexEntry};
use crate::object::{Object, ObjectType};
use crate::odb::Odb;
use crate::oid::ObjectId;
use crate::refs::{validate_refname, Refs};
use crate::tree::Tree;
use crate::tree::TreeEntry;
use crate::worktree::{tracked_status, StatusEntry};
use crate::{RefTarget, Signature};
use globset::{GlobBuilder, GlobMatcher};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The default `.git/info/exclude` content Git writes on `init`.
const DEFAULT_INFO_EXCLUDE: &[u8] = b"# git ls-files --others --exclude-from=.git/info/exclude\n# Lines that start with '#' are comments.\n# For a project mostly in C, the following would be a good set of\n# exclude patterns (uncomment them if you want to use them):\n# *.[oa]\n# *~\n";

const MAX_LOG_COMMITS: usize = 1_000_000;
const MAX_ADD_PATHS: usize = 1_000_000;
const MAX_TREE_DEPTH: usize = 256;
static WORKTREE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An opened Git repository (loose-object scope).
pub struct Repository {
    git_dir: PathBuf,
    odb: Odb,
    refs: Refs,
}

impl Repository {
    /// Create a new Git repository, returning the opened [`Repository`].
    ///
    /// Lays down a git-compatible directory skeleton (`objects/`, `refs/`,
    /// `HEAD`, `config`, `description`, `info/exclude`). For a non-bare repo the
    /// skeleton lives under `<path>/.git`; a bare repo places it directly in
    /// `<path>`. Re-running `init` on an existing repository is safe: existing
    /// `HEAD` and `config` are preserved (matching `git init`'s reinitialize
    /// behavior), only missing pieces are created.
    ///
    /// The default branch name is `main` unless overridden. It is validated so a
    /// hostile branch name cannot escape `refs/heads/`.
    pub fn init(path: impl AsRef<Path>, bare: bool, branch: Option<&str>) -> Result<Self> {
        let path = path.as_ref();
        let git_dir = if bare {
            path.to_path_buf()
        } else {
            path.join(".git")
        };

        let branch = branch.unwrap_or("main");
        // Reject a branch name that would produce an invalid ref path.
        let head_ref = format!("refs/heads/{branch}");
        validate_refname(&head_ref)?;

        // Directory skeleton. `create_dir_all` is idempotent, so reinitializing
        // an existing repository never fails on already-present directories.
        for dir in [
            git_dir.join("objects").join("info"),
            git_dir.join("objects").join("pack"),
            git_dir.join("refs").join("heads"),
            git_dir.join("refs").join("tags"),
            git_dir.join("info"),
        ] {
            std::fs::create_dir_all(&dir)?;
        }

        // HEAD points at the (not-yet-existing) default branch. Preserve an
        // existing HEAD so reinitialization does not move the current branch.
        write_if_absent(
            &git_dir.join("HEAD"),
            format!("ref: {head_ref}\n").as_bytes(),
        )?;

        // Minimal but real config. `bare` reflects the requested mode so that
        // downstream tools (and `worktree_root`) behave correctly.
        let config = format!(
            "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = {bare}\n\tlogallrefupdates = {logallrefupdates}\n",
            logallrefupdates = !bare,
        );
        write_if_absent(&git_dir.join("config"), config.as_bytes())?;

        write_if_absent(
            &git_dir.join("description"),
            b"Unnamed repository; edit this file 'description' to name the repository.\n",
        )?;
        write_if_absent(&git_dir.join("info").join("exclude"), DEFAULT_INFO_EXCLUDE)?;

        Self::open_git_dir(&git_dir)
    }

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

    /// Parse and checksum-verify this repository's Git index (v2/v3).
    pub fn index(&self) -> Result<Index> {
        Index::read(&self.git_dir)
    }

    /// Compare every stage-0 index entry with the working tree. This is the
    /// bounded tracked-file core of `git status`; untracked discovery is not yet
    /// claimed.
    pub fn tracked_status(&self) -> Result<Vec<StatusEntry>> {
        let worktree = self.worktree_root()?;
        tracked_status(worktree, &self.index()?)
    }

    /// Stage files, recursive directories, and tracked deletions into a
    /// Git-compatible index while honoring repository ignore rules.
    /// Blob objects are published first; one atomic `index.lock` transaction
    /// then installs every path together, so a competing writer cannot be lost.
    pub fn add(&self, paths: &[PathBuf]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let worktree = self.worktree_root()?;
        let current = self.index().unwrap_or(Index {
            version: 2,
            entries: Vec::new(),
        });
        let expanded = expand_add_paths(worktree, paths, &current)?;
        let mut changes = Vec::with_capacity(expanded.len());
        for relative in &expanded {
            let encoded = path_bytes(relative)?;
            validate_index_path(&encoded)?;
            let full = safe_worktree_path(worktree, relative)?;
            match std::fs::symlink_metadata(&full) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&full)?;
                    let data = os_path_bytes(&target)?;
                    let oid = self.odb.write(ObjectType::Blob, &data)?;
                    changes.push(Some(index_entry(encoded, 0o120000, data.len(), oid)?));
                }
                Ok(metadata) if metadata.is_file() => {
                    let data = std::fs::read(&full)?;
                    let mode = regular_file_mode(&metadata);
                    let oid = self.odb.write(ObjectType::Blob, &data)?;
                    changes.push(Some(index_entry(encoded, mode, data.len(), oid)?));
                }
                Ok(_) => {
                    return Err(ForgeError::UnsafePath(format!(
                        "{} is not a regular file or symlink",
                        relative.display()
                    )))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let tracked = current.entries.iter().any(|entry| entry.path == encoded);
                    if !tracked {
                        return Err(error.into());
                    }
                    changes.push(None);
                }
                Err(error) => return Err(error.into()),
            }
        }

        Index::mutate(&self.git_dir, |index| {
            for (relative, change) in expanded.iter().zip(changes) {
                let encoded = path_bytes(relative)?;
                match change {
                    Some(entry) => index.upsert(entry)?,
                    None => {
                        index.remove_path(&encoded);
                    }
                }
            }
            Ok(expanded.len())
        })
    }

    /// Write the current stage-0 index as canonical recursive Git trees.
    pub fn write_tree(&self) -> Result<ObjectId> {
        let index = self.index()?;
        if index.entries.iter().any(|entry| entry.stage() != 0) {
            return Err(ForgeError::InvalidIndex(
                "cannot write a tree with unresolved index stages".into(),
            ));
        }
        let mut root = TreeNode::default();
        for entry in &index.entries {
            root.insert(&entry.path, entry)?;
        }
        root.write(&self.odb)
    }

    /// Create a commit from the index and atomically advance symbolic HEAD.
    /// Objects are immutable and may safely remain unreachable if the ref CAS
    /// loses to another writer.
    pub fn commit_index(
        &self,
        message: impl AsRef<[u8]>,
        author: Signature,
        committer: Signature,
    ) -> Result<ObjectId> {
        let message = message.as_ref();
        if message.is_empty() {
            return Err(ForgeError::MalformedObject {
                kind: "commit",
                reason: "empty commit message".into(),
            });
        }
        let RefTarget::Symbolic(head_ref) = self.refs.read_head()? else {
            return Err(ForgeError::InvalidRef(
                "committing on detached HEAD is not supported yet".into(),
            ));
        };
        let expected = match self.refs.resolve(&head_ref) {
            Ok(oid) => Some(oid),
            Err(ForgeError::InvalidRef(_)) => None,
            Err(error) => return Err(error),
        };
        let tree = self.write_tree()?;
        // Clone before moving into the Commit struct so we can use the fields
        // for the reflog entry below.
        let committer_for_log = committer.clone();
        let commit = Commit {
            tree,
            parents: expected.into_iter().collect(),
            author,
            committer,
            extra_headers: Vec::new(),
            message: message.to_vec(),
        };
        let oid = self.odb.write(ObjectType::Commit, &commit.serialize())?;
        self.refs.compare_and_swap(&head_ref, expected, oid)?;
        // Reflog: "commit (initial): <first line>" or "commit: <first line>".
        let first_line = commit
            .message_str()
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let reflog_msg = if expected.is_none() {
            format!("commit (initial): {first_line}")
        } else {
            format!("commit: {first_line}")
        };
        // Reflog failure is non-fatal: the commit object + ref are already
        // durably written, so we surface the error but don't roll back.
        self.refs.append_reflog(
            &head_ref,
            expected,
            oid,
            &committer_for_log.name,
            &committer_for_log.email,
            committer_for_log.time,
            &committer_for_log.tz,
            &reflog_msg,
        )?;
        Ok(oid)
    }

    /// Create a commit object from an explicit tree + parents and, optionally,
    /// advance a ref to it with an atomic compare-and-swap. This is the
    /// `git commit-tree` (+ optional ref update) primitive, and unlike
    /// [`commit_index`](Self::commit_index) it needs **no worktree** — so a
    /// forge can seed or accept commits directly into a hosted **bare**
    /// repository.
    ///
    /// The commit object is written first (immutable, content-addressed, safe to
    /// leave unreachable if the ref CAS loses a race). When `update_ref` is set,
    /// the ref is advanced from whatever it currently resolves to (or from
    /// "absent" for a brand-new branch) to the new commit; a concurrent writer
    /// that moved the ref causes a [`ForgeError::RefConflict`], never a lost
    /// update.
    pub fn commit_tree(
        &self,
        tree: ObjectId,
        parents: Vec<ObjectId>,
        author: Signature,
        committer: Signature,
        message: &[u8],
        update_ref: Option<&str>,
    ) -> Result<ObjectId> {
        if message.is_empty() {
            return Err(ForgeError::MalformedObject {
                kind: "commit",
                reason: "empty commit message".into(),
            });
        }
        // The tree must exist and actually be a tree before we bless it into a
        // commit — never mint a commit that points at a missing or wrong object.
        match self.odb.read(&tree)? {
            Object::Tree(_) => {}
            other => return Err(type_error(&tree, "tree", other)),
        }
        // Clone before moving into Commit so committer fields are available
        // for the reflog entry below.
        let committer_for_log = committer.clone();
        let commit = Commit {
            tree,
            parents,
            author,
            committer,
            extra_headers: Vec::new(),
            message: message.to_vec(),
        };
        let oid = self.odb.write(ObjectType::Commit, &commit.serialize())?;
        if let Some(refname) = update_ref {
            validate_refname(refname)?;
            let expected = match self.refs.resolve(refname) {
                Ok(oid) => Some(oid),
                Err(ForgeError::InvalidRef(_)) => None,
                Err(error) => return Err(error),
            };
            self.refs.compare_and_swap(refname, expected, oid)?;
            // Reflog for the explicit ref update.
            let first_line = std::str::from_utf8(message)
                .ok()
                .and_then(|s| s.lines().next())
                .unwrap_or("")
                .to_string();
            let reflog_msg = if expected.is_none() {
                format!("commit (initial): {first_line}")
            } else {
                format!("commit: {first_line}")
            };
            self.refs.append_reflog(
                refname,
                expected,
                oid,
                &committer_for_log.name,
                &committer_for_log.email,
                committer_for_log.time,
                &committer_for_log.tz,
                &reflog_msg,
            )?;
        }
        Ok(oid)
    }

    /// Safely materialize a tree snapshot and synchronize the index. This is a
    /// plumbing checkout: it deliberately does not move HEAD. Locally modified
    /// tracked paths and untracked collisions are rejected before any mutation.
    pub fn checkout_tree(&self, tree_oid: ObjectId) -> Result<usize> {
        let worktree = self.worktree_root()?;
        let target = self.index_from_tree(tree_oid)?;
        let target_data = self.load_index_data(&target)?;
        Index::mutate(&self.git_dir, |current| {
            let statuses = tracked_status(worktree, current)?;
            let status_by_path: BTreeMap<Vec<u8>, _> = statuses
                .into_iter()
                .map(|status| (status.path, status.state))
                .collect();
            let current_by_path: BTreeMap<Vec<u8>, &IndexEntry> = current
                .entries
                .iter()
                .filter(|entry| entry.stage() == 0)
                .map(|entry| (entry.path.clone(), entry))
                .collect();
            let target_by_path: BTreeMap<Vec<u8>, &IndexEntry> = target
                .entries
                .iter()
                .map(|entry| (entry.path.clone(), entry))
                .collect();

            for (path, entry) in &current_by_path {
                let changes_path = target_by_path.get(path).map_or(true, |target| {
                    target.mode != entry.mode || target.oid != entry.oid
                });
                if changes_path
                    && status_by_path
                        .get(path)
                        .is_some_and(|state| *state != crate::worktree::WorktreeState::Unmodified)
                {
                    return Err(ForgeError::WorktreeCollision(format!(
                        "{} has local changes",
                        String::from_utf8_lossy(path)
                    )));
                }
            }

            for (path, target_entry) in &target_by_path {
                let changed = current_by_path.get(path).map_or(true, |current| {
                    current.mode != target_entry.mode || current.oid != target_entry.oid
                });
                if !changed {
                    continue;
                }
                let relative = bytes_path(path)?;
                let full = worktree.join(&relative);
                preflight_parent_chain(worktree, &relative)?;
                if !current_by_path.contains_key(path) && std::fs::symlink_metadata(&full).is_ok() {
                    return Err(ForgeError::WorktreeCollision(format!(
                        "{} would overwrite an untracked path",
                        relative.display()
                    )));
                }
                if std::fs::symlink_metadata(&full).is_ok_and(|metadata| metadata.is_dir()) {
                    return Err(ForgeError::WorktreeCollision(format!(
                        "{} is a directory",
                        relative.display()
                    )));
                }
            }

            let mut deletions: Vec<Vec<u8>> = current_by_path
                .keys()
                .filter(|path| !target_by_path.contains_key(*path))
                .cloned()
                .collect();
            deletions.sort_by_key(|path| std::cmp::Reverse(path.len()));
            for path in deletions {
                remove_tracked_path(worktree, &bytes_path(&path)?)?;
            }

            let mut written = 0;
            for (path, target_entry) in &target_by_path {
                let changed = current_by_path.get(path).map_or(true, |current| {
                    current.mode != target_entry.mode || current.oid != target_entry.oid
                });
                if changed {
                    let data = target_data
                        .get(path)
                        .ok_or_else(|| ForgeError::ObjectNotFound(target_entry.oid.to_hex()))?;
                    materialize_entry(worktree, &bytes_path(path)?, target_entry.mode, data)?;
                    written += 1;
                }
            }
            *current = target.clone();
            Ok(written)
        })
    }

    /// Restore explicit files/directories from the current index. This is the
    /// intentional-overwrite operation; repository path confinement and atomic
    /// per-file replacement still apply.
    pub fn restore_index(&self, paths: &[PathBuf]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let worktree = self.worktree_root()?;
        Index::with_locked(&self.git_dir, |index| {
            let mut selected = BTreeMap::new();
            for input in paths {
                let prefix = normalize_add_input(input)?;
                for entry in index.entries.iter().filter(|entry| entry.stage() == 0) {
                    let path = bytes_path(&entry.path)?;
                    if path == prefix
                        || (prefix.as_os_str().is_empty() || path.starts_with(&prefix))
                    {
                        selected.insert(entry.path.clone(), entry);
                    }
                }
            }
            if selected.is_empty() {
                return Err(ForgeError::InvalidIndex(
                    "restore path is not present in the index".into(),
                ));
            }
            for path in selected.keys() {
                preflight_parent_chain(worktree, &bytes_path(path)?)?;
            }
            let mut written = 0;
            for (path, entry) in selected {
                let blob = self.blob(&entry.oid)?;
                materialize_entry(worktree, &bytes_path(&path)?, entry.mode, &blob.data)?;
                written += 1;
            }
            Ok(written)
        })
    }

    fn index_from_tree(&self, tree_oid: ObjectId) -> Result<Index> {
        let mut entries = Vec::new();
        self.flatten_tree(tree_oid, Vec::new(), 0, &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Index {
            version: 2,
            entries,
        })
    }

    fn flatten_tree(
        &self,
        tree_oid: ObjectId,
        prefix: Vec<u8>,
        depth: usize,
        out: &mut Vec<IndexEntry>,
    ) -> Result<()> {
        if depth > MAX_TREE_DEPTH {
            return Err(ForgeError::ResourceLimit(format!(
                "tree depth exceeds {MAX_TREE_DEPTH}"
            )));
        }
        for entry in self.tree(&tree_oid)?.entries {
            if entry.name.is_empty() || entry.name.contains(&b'/') {
                return Err(ForgeError::UnsafePath(
                    String::from_utf8_lossy(&entry.name).into_owned(),
                ));
            }
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&entry.name);
            validate_index_path(&path)?;
            match entry.kind() {
                "tree" => self.flatten_tree(entry.oid, path, depth + 1, out)?,
                "commit" => {
                    return Err(ForgeError::InvalidIndex(
                        "submodule checkout is not supported".into(),
                    ))
                }
                _ => {
                    let mode = u32::from_str_radix(&entry.mode, 8).map_err(|_| {
                        ForgeError::MalformedObject {
                            kind: "tree",
                            reason: "invalid entry mode".into(),
                        }
                    })?;
                    let size = self.blob(&entry.oid)?.data.len();
                    out.push(index_entry(path, mode, size, entry.oid)?);
                    if out.len() > MAX_ADD_PATHS {
                        return Err(ForgeError::ResourceLimit(format!(
                            "tree exceeds {MAX_ADD_PATHS} paths"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn load_index_data(&self, index: &Index) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        index
            .entries
            .iter()
            .map(|entry| Ok((entry.path.clone(), self.blob(&entry.oid)?.data)))
            .collect()
    }

    fn worktree_root(&self) -> Result<&Path> {
        if self.git_dir.file_name().is_some_and(|name| name == ".git") {
            return self
                .git_dir
                .parent()
                .ok_or_else(|| ForgeError::NoWorktree(self.git_dir.display().to_string()));
        }
        Err(ForgeError::NoWorktree(self.git_dir.display().to_string()))
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

    // --- branch operations ---------------------------------------------------

    /// Create a new branch `name` pointing at `start_oid`.
    ///
    /// When `force` is false (the default) the branch must not already exist.
    /// When `force` is true the branch is moved even if it already points
    /// somewhere else (equivalent to `git branch -f`).
    ///
    /// Writes a reflog entry using the supplied actor identity and message.
    pub fn branch_create(
        &self,
        name: &str,
        start_oid: ObjectId,
        force: bool,
        actor_name: &str,
        actor_email: &str,
        timestamp: i64,
        tz: &str,
        message: &str,
    ) -> Result<()> {
        let refname = format!("refs/heads/{name}");
        validate_refname(&refname)?;
        let expected = match self.refs.resolve(&refname) {
            Ok(oid) => {
                if !force {
                    return Err(ForgeError::InvalidRef(format!(
                        "branch {name} already exists (use force to move it)"
                    )));
                }
                Some(oid)
            }
            Err(ForgeError::InvalidRef(_)) => None,
            Err(e) => return Err(e),
        };
        self.refs.compare_and_swap(&refname, expected, start_oid)?;
        self.refs.append_reflog(
            &refname,
            expected,
            start_oid,
            actor_name,
            actor_email,
            timestamp,
            tz,
            message,
        )?;
        Ok(())
    }

    /// Delete branch `name`.
    ///
    /// When `force` is false (equivalent to `git branch -d`) the operation
    /// fails if the branch tip is not an ancestor of HEAD (i.e. the branch
    /// has unmerged commits).  When `force` is true (`git branch -D`) the
    /// check is skipped.
    ///
    /// No reflog entry is written on deletion (git also leaves the log file
    /// intact; it is pruned by `git reflog expire`).
    pub fn branch_delete(&self, name: &str, force: bool) -> Result<()> {
        let refname = format!("refs/heads/{name}");
        validate_refname(&refname)?;
        let tip = self.refs.resolve(&refname)?;

        if !force {
            // Safety check: branch tip must be reachable from HEAD.
            if let Ok(head_oid) = self.refs.resolve_head() {
                let merged = crate::fsck::is_ancestor(
                    self.odb(),
                    tip,
                    head_oid,
                    crate::fsck::ConnectivityLimits::default(),
                )
                .unwrap_or(false);
                if !merged {
                    return Err(ForgeError::NonFastForward {
                        name: name.to_string(),
                        old: tip.to_hex(),
                        new: head_oid.to_hex(),
                    });
                }
            }
        }

        // Guard: refuse to delete the currently-checked-out branch.
        if let Ok(crate::RefTarget::Symbolic(head_target)) = self.refs.read_head() {
            if head_target == refname {
                return Err(ForgeError::InvalidRef(format!(
                    "cannot delete branch {name}: it is currently checked out"
                )));
            }
        }

        self.refs.compare_and_delete(&refname, tip)?;
        Ok(())
    }

    /// List all local branches as `(short_name, tip_oid, is_current)` triples,
    /// sorted alphabetically by name.
    ///
    /// `is_current` is true when HEAD is a symbolic ref pointing at that branch.
    pub fn branch_list(&self) -> Result<Vec<(String, ObjectId, bool)>> {
        let current = match self.refs.read_head()? {
            crate::RefTarget::Symbolic(name) => Some(name),
            crate::RefTarget::Direct(_) => None,
        };
        let all = self.refs.list()?;
        let mut branches: Vec<(String, ObjectId, bool)> = all
            .into_iter()
            .filter_map(|(refname, oid)| {
                refname.strip_prefix("refs/heads/").map(|short| {
                    let is_current = current.as_deref().map(|c| c == refname).unwrap_or(false);
                    (short.to_string(), oid, is_current)
                })
            })
            .collect();
        branches.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(branches)
    }

    /// Rename branch `old_name` to `new_name`.
    ///
    /// When `force` is false the destination must not already exist.
    /// Writes reflog entries on both the new branch and (if HEAD points at it)
    /// on HEAD.
    pub fn branch_rename(
        &self,
        old_name: &str,
        new_name: &str,
        force: bool,
        actor_name: &str,
        actor_email: &str,
        timestamp: i64,
        tz: &str,
    ) -> Result<()> {
        let old_ref = format!("refs/heads/{old_name}");
        let new_ref = format!("refs/heads/{new_name}");
        validate_refname(&old_ref)?;
        validate_refname(&new_ref)?;
        let tip = self.refs.resolve(&old_ref)?;

        // Create the new ref first (CAS on "absent").
        let new_expected = match self.refs.resolve(&new_ref) {
            Ok(oid) => {
                if !force {
                    return Err(ForgeError::InvalidRef(format!(
                        "branch {new_name} already exists (use force to overwrite)"
                    )));
                }
                Some(oid)
            }
            Err(ForgeError::InvalidRef(_)) => None,
            Err(e) => return Err(e),
        };
        self.refs.compare_and_swap(&new_ref, new_expected, tip)?;

        let rename_msg = format!("branch: renamed refs/heads/{old_name} to refs/heads/{new_name}");
        self.refs.append_reflog(
            &new_ref,
            None, // treat as creation of the new name
            tip,
            actor_name,
            actor_email,
            timestamp,
            tz,
            &rename_msg,
        )?;

        // If HEAD points at the old ref, update it to the new one.
        if let Ok(crate::RefTarget::Symbolic(head_target)) = self.refs.read_head() {
            if head_target == old_ref {
                let head_path = self.git_dir.join("HEAD");
                let lock_path = head_path.with_extension("lock");
                let mut lock = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock_path)
                    .map_err(|_| ForgeError::RefLocked("HEAD".into()))?;
                let result = (|| {
                    writeln!(lock, "ref: {new_ref}")?;
                    lock.sync_all()?;
                    std::fs::rename(&lock_path, &head_path)?;
                    Ok::<_, ForgeError>(())
                })();
                if result.is_err() {
                    let _ = std::fs::remove_file(&lock_path);
                    result?;
                }
            }
        }

        // Remove the old ref.
        self.refs.compare_and_delete(&old_ref, tip)?;
        Ok(())
    }

    // --- tag operations ------------------------------------------------------

    /// List all tags as `(name, oid)` pairs sorted alphabetically.
    ///
    /// `oid` is the tag object OID for annotated tags, or the commit/blob/tree
    /// OID for lightweight tags (matching `git show-ref --tags`).
    pub fn tag_list(&self) -> Result<Vec<(String, ObjectId)>> {
        let all = self.refs.list()?;
        let mut tags: Vec<(String, ObjectId)> = all
            .into_iter()
            .filter_map(|(refname, oid)| {
                refname
                    .strip_prefix("refs/tags/")
                    .map(|short| (short.to_string(), oid))
            })
            .collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(tags)
    }

    /// Create a **lightweight** tag: a ref in `refs/tags/<name>` pointing
    /// directly at `target_oid` (no tag object is written).
    ///
    /// When `force` is false the tag must not already exist.
    pub fn tag_create_lightweight(
        &self,
        name: &str,
        target_oid: ObjectId,
        force: bool,
    ) -> Result<()> {
        let refname = format!("refs/tags/{name}");
        validate_refname(&refname)?;
        let expected = match self.refs.resolve(&refname) {
            Ok(oid) => {
                if !force {
                    return Err(ForgeError::InvalidRef(format!(
                        "tag {name} already exists (use force to replace)"
                    )));
                }
                Some(oid)
            }
            Err(ForgeError::InvalidRef(_)) => None,
            Err(e) => return Err(e),
        };
        self.refs.compare_and_swap(&refname, expected, target_oid)?;
        Ok(())
    }

    /// Create an **annotated** tag: write a `tag` object pointing at
    /// `target_oid`, then create a ref in `refs/tags/<name>` pointing at the
    /// tag object.
    ///
    /// When `force` is false the tag must not already exist.
    ///
    /// Returns the OID of the written tag object.
    pub fn tag_create_annotated(
        &self,
        name: &str,
        target_oid: ObjectId,
        target_type: crate::object::ObjectType,
        message: &[u8],
        tagger: Signature,
        force: bool,
    ) -> Result<ObjectId> {
        let refname = format!("refs/tags/{name}");
        validate_refname(&refname)?;

        let tag_obj = crate::tag::Tag {
            object: target_oid,
            object_type: target_type,
            tag: name.to_string(),
            tagger: Some(tagger),
            extra_headers: Vec::new(),
            message: message.to_vec(),
        };
        let tag_oid = self.odb.write(ObjectType::Tag, &tag_obj.serialize())?;

        let expected = match self.refs.resolve(&refname) {
            Ok(oid) => {
                if !force {
                    return Err(ForgeError::InvalidRef(format!(
                        "tag {name} already exists (use force to replace)"
                    )));
                }
                Some(oid)
            }
            Err(ForgeError::InvalidRef(_)) => None,
            Err(e) => return Err(e),
        };
        self.refs.compare_and_swap(&refname, expected, tag_oid)?;
        Ok(tag_oid)
    }

    /// Delete tag `name`.
    ///
    /// When `force` is true the check that the ref currently exists is still
    /// performed (you cannot delete what isn't there), but the merge-safety
    /// check is bypassed.  For tags, force-delete (`git tag -d` always acts
    /// like `-D`) is the default — the boolean is accepted for API symmetry
    /// with `branch_delete`.
    pub fn tag_delete(&self, name: &str) -> Result<()> {
        let refname = format!("refs/tags/{name}");
        validate_refname(&refname)?;
        let tip = self
            .refs
            .resolve(&refname)
            .map_err(|_| ForgeError::InvalidRef(format!("tag {name} not found")))?;
        self.refs.compare_and_delete(&refname, tip)?;
        Ok(())
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
            if seen.len() > MAX_LOG_COMMITS {
                return Err(ForgeError::ResourceLimit(format!(
                    "history walk exceeds {MAX_LOG_COMMITS} commits"
                )));
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

#[derive(Default)]
struct TreeNode {
    files: BTreeMap<Vec<u8>, (String, ObjectId)>,
    directories: BTreeMap<Vec<u8>, TreeNode>,
}

impl TreeNode {
    fn insert(&mut self, path: &[u8], entry: &IndexEntry) -> Result<()> {
        let mut parts = path.split(|byte| *byte == b'/').peekable();
        let mut node = self;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                if node.directories.contains_key(part) {
                    return Err(ForgeError::InvalidIndex(
                        "file/directory path collision".into(),
                    ));
                }
                node.files
                    .insert(part.to_vec(), (format!("{:o}", entry.mode), entry.oid));
            } else {
                if node.files.contains_key(part) {
                    return Err(ForgeError::InvalidIndex(
                        "file/directory path collision".into(),
                    ));
                }
                node = node.directories.entry(part.to_vec()).or_default();
            }
        }
        Ok(())
    }

    fn write(&self, odb: &Odb) -> Result<ObjectId> {
        let mut entries = Vec::with_capacity(self.files.len() + self.directories.len());
        for (name, (mode, oid)) in &self.files {
            entries.push(TreeEntry {
                mode: mode.clone(),
                name: name.clone(),
                oid: *oid,
            });
        }
        for (name, child) in &self.directories {
            entries.push(TreeEntry {
                mode: "40000".into(),
                name: name.clone(),
                oid: child.write(odb)?,
            });
        }
        let mut tree = Tree { entries };
        tree.canonicalize();
        odb.write(ObjectType::Tree, &tree.serialize())
    }
}

/// Write `content` to `path` only if the file does not already exist, so
/// reinitializing a repository never clobbers an existing HEAD or config.
fn write_if_absent(path: &Path, content: &[u8]) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(content)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn expand_add_paths(worktree: &Path, inputs: &[PathBuf], index: &Index) -> Result<Vec<PathBuf>> {
    let mut visible = BTreeSet::new();
    let mut rules = Vec::new();
    load_ignore_file(worktree, &worktree.join(".git/info/exclude"), &mut rules)?;
    collect_visible_paths(worktree, worktree, &rules, &mut visible)?;

    let tracked: Vec<PathBuf> = index
        .entries
        .iter()
        .filter(|entry| entry.stage() == 0)
        .map(|entry| bytes_path(&entry.path))
        .collect::<Result<_>>()?;
    let mut expanded = BTreeSet::new();
    for input in inputs {
        let relative = normalize_add_input(input)?;
        let full = if relative.as_os_str().is_empty() {
            worktree.to_path_buf()
        } else {
            safe_worktree_path(worktree, &relative)?
        };
        match std::fs::symlink_metadata(&full) {
            Ok(metadata) if metadata.is_dir() => {
                expanded.extend(
                    visible
                        .iter()
                        .filter(|path| {
                            relative.as_os_str().is_empty() || path.starts_with(&relative)
                        })
                        .cloned(),
                );
                expanded.extend(
                    tracked
                        .iter()
                        .filter(|path| {
                            (relative.as_os_str().is_empty() || path.starts_with(&relative))
                                && !worktree.join(path).exists()
                        })
                        .cloned(),
                );
            }
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                let encoded = path_bytes(&relative)?;
                let is_tracked = index.entries.iter().any(|entry| entry.path == encoded);
                if visible.contains(&relative) || is_tracked {
                    expanded.insert(relative);
                }
            }
            Ok(_) => return Err(ForgeError::UnsafePath(input.display().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let before = expanded.len();
                expanded.extend(
                    tracked
                        .iter()
                        .filter(|path| *path == &relative || path.starts_with(&relative))
                        .cloned(),
                );
                if expanded.len() == before {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(expanded.into_iter().collect())
}

#[derive(Clone)]
struct IgnoreRule {
    base: PathBuf,
    matcher: GlobMatcher,
    include: bool,
    directory_only: bool,
}

fn load_ignore_file(base: &Path, path: &Path, rules: &mut Vec<IgnoreRule>) -> Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for raw in content.lines() {
        let mut pattern = raw.trim_end();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        let include = pattern.starts_with('!');
        if include {
            pattern = &pattern[1..];
        }
        if pattern.is_empty() {
            continue;
        }
        let directory_only = pattern.ends_with('/');
        pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        let normalized = if pattern.contains('/') {
            pattern.to_string()
        } else {
            format!("**/{pattern}")
        };
        let matcher = GlobBuilder::new(&normalized)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| ForgeError::InvalidIndex(format!("invalid ignore rule: {error}")))?
            .compile_matcher();
        rules.push(IgnoreRule {
            base: base.to_path_buf(),
            matcher,
            include,
            directory_only,
        });
    }
    Ok(())
}

fn ignored(path: &Path, is_dir: bool, rules: &[IgnoreRule]) -> bool {
    let mut ignored = false;
    for rule in rules {
        let Ok(relative) = path.strip_prefix(&rule.base) else {
            continue;
        };
        let candidate = relative.to_string_lossy().replace('\\', "/");
        if rule.matcher.is_match(&candidate) && (!rule.directory_only || is_dir) {
            ignored = !rule.include;
        }
    }
    ignored
}

fn collect_visible_paths(
    worktree: &Path,
    directory: &Path,
    inherited: &[IgnoreRule],
    visible: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let mut rules = Vec::from(inherited);
    load_ignore_file(directory, &directory.join(".gitignore"), &mut rules)?;
    let mut entries: Vec<_> = std::fs::read_dir(directory)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path == worktree.join(".git") {
            continue;
        }
        let kind = entry.file_type()?;
        if ignored(&path, kind.is_dir(), &rules) {
            continue;
        }
        if kind.is_dir() {
            collect_visible_paths(worktree, &path, &rules, visible)?;
        } else if kind.is_file() || kind.is_symlink() {
            let relative = path
                .strip_prefix(worktree)
                .map_err(|_| ForgeError::UnsafePath(path.display().to_string()))?;
            visible.insert(relative.to_path_buf());
            if visible.len() > MAX_ADD_PATHS {
                return Err(ForgeError::ResourceLimit(format!(
                    "recursive add exceeds {MAX_ADD_PATHS} paths"
                )));
            }
        }
    }
    Ok(())
}

fn normalize_add_input(path: &Path) -> Result<PathBuf> {
    if path == Path::new(".") {
        return Ok(PathBuf::new());
    }
    normalize_worktree_path(path)
}

#[cfg(unix)]
fn bytes_path(path: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
}

#[cfg(not(unix))]
fn bytes_path(path: &[u8]) -> Result<PathBuf> {
    String::from_utf8(path.to_vec())
        .map(PathBuf::from)
        .map_err(|_| ForgeError::UnsafePath("non-UTF-8 path unsupported".into()))
}

fn normalize_worktree_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(ForgeError::UnsafePath(path.display().to_string()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ForgeError::UnsafePath(path.display().to_string()))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ForgeError::UnsafePath(path.display().to_string()));
    }
    Ok(normalized)
}

fn safe_worktree_path(worktree: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = worktree.to_path_buf();
    let count = relative.components().count();
    for (position, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err(ForgeError::UnsafePath(relative.display().to_string()));
        };
        current.push(part);
        if position + 1 < count {
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ForgeError::UnsafePath(relative.display().to_string()))
                }
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(ForgeError::UnsafePath(relative.display().to_string())),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(current)
}

fn preflight_parent_chain(worktree: &Path, relative: &Path) -> Result<()> {
    let mut current = worktree.to_path_buf();
    let count = relative.components().count();
    for (position, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err(ForgeError::UnsafePath(relative.display().to_string()));
        };
        current.push(part);
        if position + 1 == count {
            break;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ForgeError::UnsafePath(relative.display().to_string()))
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(ForgeError::WorktreeCollision(format!(
                    "{} blocks a directory",
                    current.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_parent_directories(worktree: &Path, relative: &Path) -> Result<PathBuf> {
    let parent = relative
        .parent()
        .ok_or_else(|| ForgeError::UnsafePath(relative.display().to_string()))?;
    let mut current = worktree.to_path_buf();
    for component in parent.components() {
        let Component::Normal(part) = component else {
            return Err(ForgeError::UnsafePath(relative.display().to_string()));
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ForgeError::UnsafePath(relative.display().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                if let Some(parent) = current.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(worktree.join(relative))
}

fn materialize_entry(worktree: &Path, relative: &Path, mode: u32, data: &[u8]) -> Result<()> {
    validate_index_path(&path_bytes(relative)?)?;
    preflight_parent_chain(worktree, relative)?;
    let destination = ensure_parent_directories(worktree, relative)?;
    if std::fs::symlink_metadata(&destination).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(ForgeError::WorktreeCollision(format!(
            "{} is a directory",
            relative.display()
        )));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| ForgeError::UnsafePath(relative.display().to_string()))?;
    let sequence = WORKTREE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".rit-checkout-{}-{sequence}.tmp",
        std::process::id()
    ));
    let result = if mode == 0o120000 {
        create_checkout_symlink(&temporary, data)
    } else if matches!(mode, 0o100644 | 0o100755) {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(data)?;
        set_checkout_permissions(&file, mode)?;
        file.sync_all()?;
        drop(file);
        Ok(())
    } else {
        Err(ForgeError::InvalidIndex(format!(
            "unsupported checkout mode {mode:o}"
        )))
    };
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, &destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_tracked_path(worktree: &Path, relative: &Path) -> Result<()> {
    preflight_parent_chain(worktree, relative)?;
    let full = worktree.join(relative);
    match std::fs::symlink_metadata(&full) {
        Ok(metadata) if !metadata.is_dir() => std::fs::remove_file(&full)?,
        Ok(_) => {
            return Err(ForgeError::WorktreeCollision(format!(
                "{} became a directory",
                relative.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let mut parent = full.parent();
    while let Some(directory) = parent {
        if directory == worktree {
            break;
        }
        match std::fs::remove_dir(directory) {
            Ok(()) => parent = directory.parent(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) =>
            {
                break
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_checkout_symlink(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(data), path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_checkout_symlink(_path: &Path, _data: &[u8]) -> Result<()> {
    Err(ForgeError::UnsafePath(
        "symlink checkout is unsupported on this platform".into(),
    ))
}

#[cfg(unix)]
fn set_checkout_permissions(file: &std::fs::File, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(if mode == 0o100755 {
        0o755
    } else {
        0o644
    }))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_checkout_permissions(_file: &std::fs::File, _mode: u32) -> Result<()> {
    Ok(())
}

fn index_entry(path: Vec<u8>, mode: u32, size: usize, oid: ObjectId) -> Result<IndexEntry> {
    let file_size = u32::try_from(size)
        .map_err(|_| ForgeError::ResourceLimit("index file exceeds u32 size".into()))?;
    Ok(IndexEntry {
        mode,
        file_size,
        oid,
        flags: path.len().min(0x0fff) as u16,
        path,
    })
}

#[cfg(unix)]
fn regular_file_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    }
}

#[cfg(not(unix))]
fn regular_file_mode(_: &std::fs::Metadata) -> u32 {
    0o100644
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = Vec::new();
    for (index, component) in path.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err(ForgeError::UnsafePath(path.display().to_string()));
        };
        if index > 0 {
            bytes.push(b'/');
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<Vec<u8>> {
    path.to_str()
        .map(|value| value.replace('\\', "/").into_bytes())
        .ok_or_else(|| ForgeError::UnsafePath("non-UTF-8 path unsupported".into()))
}

#[cfg(unix)]
fn os_path_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn os_path_bytes(path: &Path) -> Result<Vec<u8>> {
    path.to_str()
        .map(|value| value.as_bytes().to_vec())
        .ok_or_else(|| ForgeError::UnsafePath("non-UTF-8 symlink target unsupported".into()))
}

/// Build a helpful "wrong type" error.
fn type_error(oid: &ObjectId, expected: &'static str, got: Object) -> ForgeError {
    ForgeError::malformed(
        "object",
        format!("{oid} is a {}, expected {expected}", got.object_type()),
    )
}

//! Repository browsing for the web UI: branches/tags, tree listings, blob reads,
//! commit history, single-commit diffs, and source archives — all reading real
//! objects through `rit`, never placeholder data.

use flate2::write::GzEncoder;
use flate2::Compression;
use rit::object::Object;
use rit::{ObjectId, Repository};

use crate::collab::CommitInfo;
use crate::error::{GitHostError, Result};
use crate::RepoHost;

/// One entry in a tree listing.
pub struct TreeItem {
    pub name: String,
    pub is_dir: bool,
    pub mode: String,
    pub oid: String,
}

/// A blob's bytes plus whether it looks binary.
pub struct BlobContent {
    pub bytes: Vec<u8>,
    pub binary: bool,
}

const MAX_TREE_DEPTH: usize = 256;

impl RepoHost {
    /// Branch names (`refs/heads/*`), sorted.
    pub fn branches(&self, org: &str, repo: &str) -> Result<Vec<String>> {
        self.ref_names(org, repo, "refs/heads/")
    }

    /// Tag names (`refs/tags/*`), sorted.
    pub fn tags(&self, org: &str, repo: &str) -> Result<Vec<String>> {
        self.ref_names(org, repo, "refs/tags/")
    }

    fn ref_names(&self, org: &str, repo: &str, prefix: &str) -> Result<Vec<String>> {
        let repository = self.open(org, repo)?;
        let mut names: Vec<String> = repository
            .refs()
            .list()?
            .into_iter()
            .filter_map(|(name, _)| name.strip_prefix(prefix).map(|s| s.to_string()))
            .collect();
        names.sort();
        Ok(names)
    }

    /// The default branch name (from `HEAD`), defaulting to `main`.
    pub fn default_branch(&self, org: &str, repo: &str) -> Result<String> {
        let head = String::from_utf8_lossy(&self.head(org, repo)?).into_owned();
        let name = head
            .trim()
            .strip_prefix("ref: refs/heads/")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "main".to_string());
        Ok(name)
    }

    /// Whether the repository has no branches yet.
    pub fn is_empty_repo(&self, org: &str, repo: &str) -> Result<bool> {
        Ok(self.branches(org, repo)?.is_empty())
    }

    /// Resolve a ref (branch/tag/oid) to the **commit oid** it points at,
    /// following annotated tags to their commit.
    pub fn resolve_commit(&self, org: &str, repo: &str, rev: &str) -> Result<ObjectId> {
        let repository = self.open(org, repo)?;
        let mut oid = repository
            .resolve(rev)
            .map_err(|_| GitHostError::NotFound(format!("revision {rev}")))?;
        for _ in 0..10 {
            match repository.odb().read(&oid)? {
                Object::Commit(_) => return Ok(oid),
                Object::Tag(tag) => oid = tag.object,
                _ => return Err(GitHostError::Invalid(format!("{rev} is not a commit"))),
            }
        }
        Err(GitHostError::Invalid("tag chain too deep".into()))
    }

    /// List a tree at `commit`/`path` (empty path = root).
    pub fn list_tree(
        &self,
        org: &str,
        repo: &str,
        commit: ObjectId,
        path: &str,
    ) -> Result<Vec<TreeItem>> {
        let repository = self.open(org, repo)?;
        let tree_oid = self.navigate_to_tree(&repository, commit, path)?;
        let tree = repository.tree(&tree_oid)?;
        let mut items: Vec<TreeItem> = tree
            .entries
            .iter()
            .map(|e| TreeItem {
                name: e.name_str().into_owned(),
                is_dir: e.kind() == "tree",
                mode: e.mode.clone(),
                oid: e.oid.to_hex(),
            })
            .collect();
        // Directories first, then files; alphabetical within each.
        items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
        Ok(items)
    }

    /// Read a blob at `commit`/`path`.
    pub fn read_blob_at(
        &self,
        org: &str,
        repo: &str,
        commit: ObjectId,
        path: &str,
    ) -> Result<BlobContent> {
        let repository = self.open(org, repo)?;
        let (dir, file) = split_parent(path);
        let tree_oid = self.navigate_to_tree(&repository, commit, dir)?;
        let tree = repository.tree(&tree_oid)?;
        let entry = tree
            .entries
            .iter()
            .find(|e| e.name_str() == file && e.kind() != "tree")
            .ok_or_else(|| GitHostError::NotFound(format!("file {path}")))?;
        let bytes = repository.blob(&entry.oid)?.data;
        let binary = bytes.contains(&0);
        Ok(BlobContent { bytes, binary })
    }

    /// Commit history from `commit`, newest first, capped at `limit`.
    pub fn history(
        &self,
        org: &str,
        repo: &str,
        commit: ObjectId,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        let repository = self.open(org, repo)?;
        let log = repository.log(commit)?;
        Ok(log
            .into_iter()
            .take(limit)
            .map(|(oid, c)| CommitInfo {
                oid: oid.to_hex(),
                summary: c.summary(),
                author_name: c.author.name.clone(),
                author_email: c.author.email.clone(),
                message: c.message_str().into_owned(),
                time: c.author.time,
            })
            .collect())
    }

    /// A single commit's metadata and its diff against its first parent (or the
    /// empty tree for a root commit).
    pub fn commit_detail(
        &self,
        org: &str,
        repo: &str,
        commit: ObjectId,
    ) -> Result<(CommitInfo, crate::DiffResult)> {
        let repository = self.open(org, repo)?;
        let c = repository.commit(&commit)?;
        let new_tree = c.tree;
        let old_tree = match c.parents.first() {
            Some(parent) => Some(repository.commit(parent)?.tree),
            None => None,
        };
        let diff = self.render_diff(&repository, old_tree, Some(new_tree))?;
        let info = CommitInfo {
            oid: commit.to_hex(),
            summary: c.summary(),
            author_name: c.author.name.clone(),
            author_email: c.author.email.clone(),
            message: c.message_str().into_owned(),
            time: c.author.time,
        };
        Ok((info, diff))
    }

    /// Build a gzip-compressed tar archive of the tree at `commit`.
    pub fn archive_targz(
        &self,
        org: &str,
        repo: &str,
        commit: ObjectId,
        prefix: &str,
    ) -> Result<Vec<u8>> {
        let repository = self.open(org, repo)?;
        let tree = repository.peel_to_commit(commit)?.tree;
        let mut files = Vec::new();
        collect_files(&repository, tree, String::new(), 0, &mut files)?;

        let gz = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (path, mode, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(if mode == "100755" { 0o755 } else { 0o644 });
            header.set_cksum();
            let full = if prefix.is_empty() {
                path
            } else {
                format!("{prefix}/{path}")
            };
            builder
                .append_data(&mut header, &full, data.as_slice())
                .map_err(GitHostError::Io)?;
        }
        let gz = builder.into_inner().map_err(GitHostError::Io)?;
        gz.finish().map_err(GitHostError::Io)
    }

    /// Navigate from a commit down a `/`-separated directory path to a tree oid.
    fn navigate_to_tree(
        &self,
        repository: &Repository,
        commit: ObjectId,
        path: &str,
    ) -> Result<ObjectId> {
        let mut tree_oid = repository.peel_to_commit(commit)?.tree;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            let tree = repository.tree(&tree_oid)?;
            let entry = tree
                .entries
                .iter()
                .find(|e| e.name_str() == component && e.kind() == "tree")
                .ok_or_else(|| GitHostError::NotFound(format!("directory {path}")))?;
            tree_oid = entry.oid;
        }
        Ok(tree_oid)
    }
}

/// Split `a/b/c.txt` into (`"a/b"`, `"c.txt"`).
fn split_parent(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, file)) => (dir, file),
        None => ("", path),
    }
}

/// Recursively collect every blob under `tree` as (path, mode, data).
fn collect_files(
    repository: &Repository,
    tree_oid: ObjectId,
    prefix: String,
    depth: usize,
    out: &mut Vec<(String, String, Vec<u8>)>,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(GitHostError::Invalid("tree too deep for archive".into()));
    }
    let tree = repository.tree(&tree_oid)?;
    for entry in tree.entries {
        let name = entry.name_str().into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.kind() == "tree" {
            collect_files(repository, entry.oid, path, depth + 1, out)?;
        } else if entry.kind() == "blob" {
            let data = repository.blob(&entry.oid)?.data;
            out.push((path, entry.mode.clone(), data));
        }
    }
    Ok(())
}

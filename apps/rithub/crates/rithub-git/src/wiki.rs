//! Flat-file edits on a bare repository (the wiki plane): upsert and delete
//! root-level files in one commit, and per-file history.

use rit::object::ObjectType;
use rit::tree::{Tree, TreeEntry};
use rit::{ObjectId, Repository, Signature};

use crate::{CommitInfo, GitHostError, RepoHost, Result, SeedFile};

/// What a commit changed.
pub struct FileEdit<'a> {
    pub upserts: &'a [SeedFile],
    pub deletes: &'a [String],
}

impl RepoHost {
    /// Apply `edit` to the root tree of `branch` and commit it (creating the
    /// branch on an empty repo). `expected_parent`, when given, must be the
    /// current tip (optimistic concurrency); the ref moves with compare-and-swap.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_files(
        &self,
        org: &str,
        repo: &str,
        branch: &str,
        edit: FileEdit<'_>,
        expected_parent: Option<&str>,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        let repository = self.open(org, repo)?;
        let branch_ref = format!("refs/heads/{branch}");
        let tip = repository.refs().resolve(&branch_ref).ok();
        if let Some(expected) = expected_parent {
            if tip.map(|t| t.to_hex()).as_deref() != Some(expected) {
                return Err(GitHostError::Invalid("the page changed since you loaded it".into()));
            }
        }
        let mut entries: Vec<TreeEntry> = match tip {
            Some(t) => repository.tree(&repository.commit(&t)?.tree)?.entries,
            None => Vec::new(),
        };
        for name in edit.deletes {
            let before = entries.len();
            entries.retain(|e| e.name_str() != name.as_str());
            if entries.len() == before {
                return Err(GitHostError::NotFound(format!("file {name}")));
            }
        }
        for file in edit.upserts {
            if file.name.is_empty() || file.name.contains('/') || file.name.contains('\0') || file.name.starts_with('.') {
                return Err(GitHostError::Invalid(format!("invalid file name {:?}", file.name)));
            }
            let oid = repository.hash_object(ObjectType::Blob, &file.content, true)?;
            entries.retain(|e| e.name_str() != file.name.as_str());
            entries.push(TreeEntry { mode: if file.executable { "100755".into() } else { "100644".into() }, name: file.name.clone().into_bytes(), oid });
        }
        let mut tree = Tree { entries };
        tree.canonicalize();
        let tree_oid = repository.hash_object(ObjectType::Tree, &tree.serialize(), true)?;
        let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let signature = Signature::parse(&format!("{author_name} <{author_email}> {seconds} +0000"), "commit")?;
        let commit = repository.commit_tree(tree_oid, tip.into_iter().collect(), signature.clone(), signature, message.as_bytes(), Some(&branch_ref))?;
        rit::update_server_info(&repository)?;
        Ok(commit.to_hex())
    }

    /// Commits (newest first) in which root file `name` was added, changed or
    /// removed, from `commit`, capped at `limit`.
    pub fn file_history(&self, org: &str, repo: &str, commit: ObjectId, name: &str, limit: usize) -> Result<Vec<CommitInfo>> {
        let repository = self.open(org, repo)?;
        let blob_at = |r: &Repository, c: &ObjectId| -> Result<Option<ObjectId>> {
            let tree = r.tree(&r.commit(c)?.tree)?;
            Ok(tree.entries.iter().find(|e| e.name_str() == name).map(|e| e.oid))
        };
        let mut out = Vec::new();
        for (oid, c) in repository.log(commit)? {
            let mine = blob_at(&repository, &oid)?;
            let parent = match c.parents.first() {
                Some(p) => blob_at(&repository, p)?,
                None => None,
            };
            if mine != parent {
                out.push(CommitInfo {
                    oid: oid.to_hex(),
                    summary: c.summary(),
                    author_name: c.author.name.clone(),
                    author_email: c.author.email.clone(),
                    message: c.message_str().into_owned(),
                    time: c.author.time,
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

//! `git show [<object>]` command handler.
//!
//! Shows the content of a Git object in a human-readable form, matching `git show`:
//!
//! | Object type | Output                                              |
//! |-------------|-----------------------------------------------------|
//! | **Commit**  | Commit metadata (like `git log -1`) followed by the |
//! |             | unified diff of the commit vs its first parent (or  |
//! |             | all-additions for a root commit).                   |
//! | **Tag**     | The raw annotated-tag payload (`git cat-file -p`).  |
//! | **Tree**    | Directory listing (`git ls-tree` format).           |
//! | **Blob**    | Raw file content (like `cat`).                      |
//!
//! The object argument may be any revision syntax accepted by
//! [`Repository::resolve`]: `HEAD`, a branch/tag name, a full 40-hex oid, or an
//! unambiguous abbreviated prefix.

use std::io::Write;

use crate::commit::Commit;
use crate::diff::{diff_trees, ChangeKind, TreeChange};
use crate::diff_render::render_file_diff;
use crate::error::Result;
use crate::object::Object;
use crate::oid::ObjectId;
use crate::repo::Repository;

/// Implements `git show [<object>]`.
///
/// When `object` is `None` the default is `HEAD`.
/// Output is written to `out`.
pub fn cmd_show(repo: &Repository, object: Option<&str>, out: &mut impl Write) -> Result<()> {
    let rev = object.unwrap_or("HEAD");
    let oid = repo.resolve(rev)?;

    match repo.read_object(&oid)? {
        Object::Commit(commit) => show_commit(repo, oid, &commit, out),
        Object::Tag(tag) => {
            // Print the annotated-tag payload verbatim (same as `git cat-file -p`).
            out.write_all(&tag.serialize())?;
            // If the tagged object is a commit, also show that commit's diff.
            let tagged_oid = tag.object;
            match repo.read_object(&tagged_oid)? {
                Object::Commit(commit) => {
                    writeln!(out)?;
                    show_commit(repo, tagged_oid, &commit, out)?;
                }
                _ => {} // non-commit tags: just show the tag payload
            }
            Ok(())
        }
        Object::Tree(tree) => {
            // ls-tree style: "<mode> <kind> <oid>\t<name>"
            for entry in &tree.entries {
                writeln!(
                    out,
                    "{} {} {}\t{}",
                    entry.display_mode(),
                    entry.kind(),
                    entry.oid,
                    entry.name_str()
                )?;
            }
            Ok(())
        }
        Object::Blob(blob) => {
            out.write_all(&blob.data)?;
            Ok(())
        }
    }
}

// ── Commit display ────────────────────────────────────────────────────────────

/// Write the full `git show` block for one commit to `out`:
/// metadata header, body, then unified diff vs first parent.
fn show_commit(
    repo: &Repository,
    oid: ObjectId,
    commit: &Commit,
    out: &mut impl Write,
) -> Result<()> {
    // ── Header ────────────────────────────────────────────────────────────────
    writeln!(out, "commit {oid}")?;

    // Merge commits get an extra "Merge:" line.
    if commit.parents.len() > 1 {
        let parents: Vec<String> = commit
            .parents
            .iter()
            .map(|p| p.to_hex()[..7].to_string())
            .collect();
        writeln!(out, "Merge: {}", parents.join(" "))?;
    }

    writeln!(
        out,
        "Author: {} <{}>",
        commit.author.name, commit.author.email
    )?;
    writeln!(out, "Date:   {} {}", commit.author.time, commit.author.tz)?;
    writeln!(out)?;

    // ── Message body ──────────────────────────────────────────────────────────
    for line in commit.message_str().lines() {
        writeln!(out, "    {line}")?;
    }
    writeln!(out)?;

    // ── Diff vs first parent (or empty tree for root commits) ─────────────────
    let parent_tree: Option<ObjectId> = match commit.parents.first() {
        Some(parent_oid) => {
            let parent_commit = repo.peel_to_commit(*parent_oid)?;
            Some(parent_commit.tree)
        }
        None => None, // root commit → diff against empty tree
    };

    let changes = diff_trees(repo.odb(), parent_tree, Some(commit.tree))?;
    for change in &changes {
        render_show_change(repo, change, out)?;
    }

    Ok(())
}

/// Render one [`TreeChange`] for `show` output (delegates to [`render_file_diff`]).
fn render_show_change(repo: &Repository, change: &TreeChange, out: &mut impl Write) -> Result<()> {
    let (old_data, new_data) = match change.kind {
        ChangeKind::Added => {
            let new_blob = repo.blob(change.new_oid.as_ref().expect("Added has new_oid"))?;
            (None, Some(new_blob.data))
        }
        ChangeKind::Deleted => {
            let old_blob = repo.blob(change.old_oid.as_ref().expect("Deleted has old_oid"))?;
            (Some(old_blob.data), None)
        }
        ChangeKind::Modified => {
            let old_blob = repo.blob(change.old_oid.as_ref().expect("Modified has old_oid"))?;
            let new_blob = repo.blob(change.new_oid.as_ref().expect("Modified has new_oid"))?;
            (Some(old_blob.data), Some(new_blob.data))
        }
    };

    render_file_diff(
        &change.path,
        change.old_oid,
        change.new_oid,
        change.old_mode.as_deref(),
        change.new_mode.as_deref(),
        old_data.as_deref(),
        new_data.as_deref(),
        out,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;
    use crate::{Signature, Tree, TreeEntry};

    fn make_repo_with_commit(
        dir: &std::path::Path,
        file: &str,
        content: &[u8],
    ) -> (Repository, ObjectId) {
        let repo = Repository::init(dir, false, Some("main")).unwrap();

        let blob = repo.hash_object(ObjectType::Blob, content, true).unwrap();
        let mut tree_obj = Tree {
            entries: vec![TreeEntry {
                mode: "100644".into(),
                name: file.as_bytes().to_vec(),
                oid: blob,
            }],
        };
        tree_obj.canonicalize();
        let tree_oid = repo
            .hash_object(ObjectType::Tree, &tree_obj.serialize(), true)
            .unwrap();

        let sig = Signature::parse("Test <t@t.com> 1700000000 +0000", "test").unwrap();
        let commit_oid = repo
            .commit_tree(
                tree_oid,
                vec![],
                sig.clone(),
                sig,
                b"root commit\n",
                Some("refs/heads/main"),
            )
            .unwrap();
        (repo, commit_oid)
    }

    #[test]
    fn show_commit_prints_header_and_diff() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = make_repo_with_commit(dir.path(), "hello.txt", b"hello\n");

        let mut out = Vec::new();
        cmd_show(&repo, None, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(text.contains("commit "), "expected 'commit' header: {text}");
        assert!(text.contains("Author: Test"), "expected author: {text}");
        assert!(text.contains("    root commit"), "expected message: {text}");
        assert!(
            text.contains("new file mode 100644"),
            "expected new file: {text}"
        );
        assert!(text.contains("+hello"), "expected diff line: {text}");
    }

    #[test]
    fn show_blob_prints_raw_content() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let content = b"raw bytes here\n";
        let oid = repo.hash_object(ObjectType::Blob, content, true).unwrap();

        let mut out = Vec::new();
        cmd_show(&repo, Some(&oid.to_hex()), &mut out).unwrap();
        assert_eq!(out, content);
    }

    #[test]
    fn show_tree_prints_ls_tree_format() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();

        let blob = repo.hash_object(ObjectType::Blob, b"x", true).unwrap();
        let mut tree_obj = Tree {
            entries: vec![TreeEntry {
                mode: "100644".into(),
                name: b"x.txt".to_vec(),
                oid: blob,
            }],
        };
        tree_obj.canonicalize();
        let tree_oid = repo
            .hash_object(ObjectType::Tree, &tree_obj.serialize(), true)
            .unwrap();

        let mut out = Vec::new();
        cmd_show(&repo, Some(&tree_oid.to_hex()), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("100644 blob"), "expected blob entry: {text}");
        assert!(text.contains("x.txt"), "expected file name: {text}");
    }

    #[test]
    fn show_commit_with_parent_shows_modification_diff() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), false, Some("main")).unwrap();

        let sig = Signature::parse("Test <t@t.com> 1700000000 +0000", "test").unwrap();

        // First commit: file.txt = "version 1\n"
        let blob1 = repo
            .hash_object(ObjectType::Blob, b"version 1\n", true)
            .unwrap();
        let mut t1 = Tree {
            entries: vec![TreeEntry {
                mode: "100644".into(),
                name: b"file.txt".to_vec(),
                oid: blob1,
            }],
        };
        t1.canonicalize();
        let tree1 = repo
            .hash_object(ObjectType::Tree, &t1.serialize(), true)
            .unwrap();
        let commit1 = repo
            .commit_tree(
                tree1,
                vec![],
                sig.clone(),
                sig.clone(),
                b"v1\n",
                Some("refs/heads/main"),
            )
            .unwrap();

        // Second commit: file.txt = "version 2\n"
        let blob2 = repo
            .hash_object(ObjectType::Blob, b"version 2\n", true)
            .unwrap();
        let mut t2 = Tree {
            entries: vec![TreeEntry {
                mode: "100644".into(),
                name: b"file.txt".to_vec(),
                oid: blob2,
            }],
        };
        t2.canonicalize();
        let tree2 = repo
            .hash_object(ObjectType::Tree, &t2.serialize(), true)
            .unwrap();
        let commit2 = repo
            .commit_tree(
                tree2,
                vec![commit1],
                sig.clone(),
                sig,
                b"v2\n",
                Some("refs/heads/main"),
            )
            .unwrap();

        let mut out = Vec::new();
        cmd_show(&repo, Some(&commit2.to_hex()), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(
            text.contains("-version 1"),
            "expected deletion line: {text}"
        );
        assert!(
            text.contains("+version 2"),
            "expected addition line: {text}"
        );
    }
}

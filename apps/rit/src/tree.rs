//! Tree objects: the directory listings of a Git repository.
//!
//! On-disk payload format is a concatenation of entries, each:
//!
//! ```text
//! <mode> <name>\0<20-byte-oid>
//! ```
//!
//! `mode` is octal ASCII with no fixed width (Git stores `40000` for a
//! sub-tree, not `040000`), `name` is raw path-component bytes, and the oid is
//! 20 raw bytes (not hex).

use crate::error::{ForgeError, Result};
use crate::oid::{ObjectId, OID_LEN};

/// A single entry (child) of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Mode string exactly as stored (e.g. `100644`, `100755`, `40000`).
    pub mode: String,
    /// Entry name (a single path component), preserved as raw bytes because
    /// Git does not require paths to be valid UTF-8.
    pub name: Vec<u8>,
    /// The child object id (blob for files, tree for directories).
    pub oid: ObjectId,
}

impl TreeEntry {
    /// The entry name as a lossy UTF-8 string, for display.
    pub fn name_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }

    /// The human `git cat-file`-style kind derived from the mode.
    pub fn kind(&self) -> &'static str {
        match self.mode.as_str() {
            "40000" | "040000" => "tree",
            "160000" => "commit", // gitlink / submodule
            _ => "blob",
        }
    }

    /// The mode left-padded to 6 digits, matching `git cat-file -p` output
    /// (Git prints `040000` for trees even though it stores `40000`).
    pub fn display_mode(&self) -> String {
        format!("{:0>6}", self.mode)
    }
}

/// A parsed tree object.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tree {
    /// Entries in stored order (Git stores them sorted).
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Parse a tree payload into entries, failing closed on truncation.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut entries = Vec::new();
        let mut pos = 0;
        while pos < payload.len() {
            // mode: bytes up to the space
            let space = payload[pos..]
                .iter()
                .position(|&b| b == b' ')
                .ok_or_else(|| ForgeError::malformed("tree", "entry missing space after mode"))?;
            let mode = std::str::from_utf8(&payload[pos..pos + space])
                .map_err(|_| ForgeError::malformed("tree", "mode is not ASCII"))?
                .to_string();
            pos += space + 1;

            // name: bytes up to the NUL
            let nul = payload[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| ForgeError::malformed("tree", "entry missing NUL after name"))?;
            let name = payload[pos..pos + nul].to_vec();
            if name.is_empty() {
                return Err(ForgeError::malformed("tree", "entry has empty name"));
            }
            pos += nul + 1;

            // oid: exactly 20 raw bytes
            if pos + OID_LEN > payload.len() {
                return Err(ForgeError::malformed("tree", "truncated entry oid"));
            }
            let mut raw = [0u8; OID_LEN];
            raw.copy_from_slice(&payload[pos..pos + OID_LEN]);
            pos += OID_LEN;

            entries.push(TreeEntry {
                mode,
                name,
                oid: ObjectId::from_bytes(raw),
            });
        }
        Ok(Self { entries })
    }

    /// Serialize back to the exact on-disk payload (round-trips Git output).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for e in &self.entries {
            out.extend_from_slice(e.mode.as_bytes());
            out.push(b' ');
            out.extend_from_slice(&e.name);
            out.push(0);
            out.extend_from_slice(e.oid.as_bytes());
        }
        out
    }

    /// Find an entry by exact name.
    pub fn entry(&self, name: &[u8]) -> Option<&TreeEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Sort entries into Git's canonical order.
    ///
    /// Git orders tree entries by name, but compares as though every tree
    /// entry's name had a trailing `/`. This is what makes `write-tree`
    /// deterministic and hash-stable.
    pub fn canonicalize(&mut self) {
        self.entries.sort_by(git_name_cmp);
    }
}

/// Compare two entries using Git's tree-sort rule (trees sort as `name/`).
fn git_name_cmp(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    let a_key = sort_key(a);
    let b_key = sort_key(b);
    a_key.cmp(&b_key)
}

fn sort_key(e: &TreeEntry) -> Vec<u8> {
    let mut key = e.name.clone();
    if e.kind() == "tree" {
        key.push(b'/');
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object;

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex).unwrap()
    }

    #[test]
    fn parse_serialize_roundtrips() {
        let entries = vec![
            TreeEntry {
                mode: "100644".into(),
                name: b"a.txt".to_vec(),
                oid: oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
            },
            TreeEntry {
                mode: "40000".into(),
                name: b"sub".to_vec(),
                oid: oid("4b825dc642cb6eb9a060e54bf8d69288fbee4904"),
            },
        ];
        let tree = Tree { entries };
        let payload = tree.serialize();
        let reparsed = Tree::parse(&payload).unwrap();
        assert_eq!(tree, reparsed);
    }

    #[test]
    fn empty_tree_matches_known_oid() {
        // The canonical empty tree oid every Git repo shares.
        let payload = Tree::default().serialize();
        let (id, _) = object::hash_content(object::ObjectType::Tree, &payload);
        assert_eq!(id.to_hex(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    }

    #[test]
    fn rejects_truncated_oid() {
        let bad = b"100644 a.txt\0short".to_vec();
        assert!(Tree::parse(&bad).is_err());
    }

    #[test]
    fn canonicalize_sorts_trees_as_slashed() {
        let mut tree = Tree {
            entries: vec![
                TreeEntry {
                    mode: "100644".into(),
                    name: b"foo".to_vec(),
                    oid: oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
                },
                TreeEntry {
                    mode: "40000".into(),
                    name: b"foo".to_vec(),
                    oid: oid("4b825dc642cb6eb9a060e54bf8d69288fbee4904"),
                },
            ],
        };
        tree.canonicalize();
        // "foo" (blob) sorts before "foo/" (tree)
        assert_eq!(tree.entries[0].kind(), "blob");
        assert_eq!(tree.entries[1].kind(), "tree");
    }
}

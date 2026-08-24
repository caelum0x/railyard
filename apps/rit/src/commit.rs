//! Commit objects: a snapshot (`tree`) plus ancestry (`parent`), authorship,
//! and a message.
//!
//! Payload layout:
//!
//! ```text
//! tree <40-hex>\n
//! parent <40-hex>\n        (zero or more, in order)
//! author <signature>\n
//! committer <signature>\n
//! [extra headers...]\n
//! \n
//! <message>
//! ```

use crate::error::{ForgeError, Result};
use crate::headers::{self, Header};
use crate::oid::ObjectId;
use crate::signature::Signature;

/// A parsed commit object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The snapshot this commit points at.
    pub tree: ObjectId,
    /// Parent commits, in stored order (empty for a root commit).
    pub parents: Vec<ObjectId>,
    /// Who wrote the change.
    pub author: Signature,
    /// Who recorded it.
    pub committer: Signature,
    /// Any other headers (e.g. `encoding`, `gpgsig`), preserved in order so
    /// re-serialization is byte-identical to Git's output.
    pub extra_headers: Vec<Header>,
    /// The commit message body.
    pub message: Vec<u8>,
}

impl Commit {
    /// Parse a commit payload, failing closed on missing required headers.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let block = headers::parse(payload, "commit")?;

        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;
        let mut extra_headers = Vec::new();

        for h in block.headers {
            match h.key.as_str() {
                "tree" => {
                    let hex = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("commit", "tree oid not ASCII"))?;
                    tree = Some(ObjectId::from_hex(hex)?);
                }
                "parent" => {
                    let hex = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("commit", "parent oid not ASCII"))?;
                    parents.push(ObjectId::from_hex(hex)?);
                }
                "author" => {
                    let v = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("commit", "author not UTF-8"))?;
                    author = Some(Signature::parse(v, "commit")?);
                }
                "committer" => {
                    let v = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("commit", "committer not UTF-8"))?;
                    committer = Some(Signature::parse(v, "commit")?);
                }
                _ => extra_headers.push(h),
            }
        }

        Ok(Self {
            tree: tree.ok_or_else(|| ForgeError::malformed("commit", "missing tree header"))?,
            parents,
            author: author
                .ok_or_else(|| ForgeError::malformed("commit", "missing author header"))?,
            committer: committer
                .ok_or_else(|| ForgeError::malformed("commit", "missing committer header"))?,
            extra_headers,
            message: block.message,
        })
    }

    /// Serialize back to the exact on-disk payload.
    ///
    /// Header order (tree, parents, author, committer, extras) matches Git, so
    /// for objects Git wrote this reproduces their bytes and thus their oid.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_header(&mut out, "tree", self.tree.to_hex().as_bytes());
        for p in &self.parents {
            push_header(&mut out, "parent", p.to_hex().as_bytes());
        }
        push_header(&mut out, "author", self.author.to_raw().as_bytes());
        push_header(&mut out, "committer", self.committer.to_raw().as_bytes());
        for h in &self.extra_headers {
            push_header(&mut out, &h.key, &h.value);
        }
        out.push(b'\n');
        out.extend_from_slice(&self.message);
        out
    }

    /// The message as a lossy UTF-8 string.
    pub fn message_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.message)
    }

    /// The first line of the message (the summary).
    pub fn summary(&self) -> String {
        self.message_str().lines().next().unwrap_or("").to_string()
    }
}

/// Append one `key value\n` header, restoring continuation-line spacing.
fn push_header(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    out.extend_from_slice(key.as_bytes());
    out.push(b' ');
    out.extend_from_slice(&headers::serialize_value(value));
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_serialize_roundtrips() {
        let payload = concat!(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
            "parent e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n",
            "author Alice <a@b.c> 1700000000 +0000\n",
            "committer Alice <a@b.c> 1700000000 +0000\n",
            "\n",
            "initial commit\n",
        )
        .as_bytes()
        .to_vec();

        let commit = Commit::parse(&payload).unwrap();
        assert_eq!(commit.parents.len(), 1);
        assert_eq!(commit.author.name, "Alice");
        assert_eq!(commit.summary(), "initial commit");
        // byte-exact round trip is the property that keeps oids stable
        assert_eq!(commit.serialize(), payload);
    }

    #[test]
    fn root_commit_has_no_parents() {
        let payload = concat!(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
            "author Alice <a@b.c> 1700000000 +0000\n",
            "committer Alice <a@b.c> 1700000000 +0000\n",
            "\n",
            "root\n",
        )
        .as_bytes()
        .to_vec();
        let commit = Commit::parse(&payload).unwrap();
        assert!(commit.parents.is_empty());
        assert_eq!(commit.serialize(), payload);
    }

    #[test]
    fn rejects_missing_tree() {
        let payload = b"author Alice <a@b.c> 1 +0000\ncommitter Alice <a@b.c> 1 +0000\n\nx\n";
        assert!(Commit::parse(payload).is_err());
    }
}

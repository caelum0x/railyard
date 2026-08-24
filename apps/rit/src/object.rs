//! Core object model: types, canonical serialization, SHA-1 hashing, and the
//! loose-object framing shared by every Git object.
//!
//! Every Git object is `"<type> <len>\0<payload>"`, and its identity is the
//! SHA-1 of *those exact bytes*. Getting this framing byte-for-byte right is
//! what makes our object ids identical to real Git's.

use crate::blob::Blob;
use crate::commit::Commit;
use crate::error::{ForgeError, Result};
use crate::oid::{ObjectId, OID_LEN};
use crate::tag::Tag;
use crate::tree::Tree;
use sha1::{Digest, Sha1};

/// The four object kinds in the loose-object model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectType {
    /// File contents.
    Blob,
    /// Directory listing.
    Tree,
    /// A history node.
    Commit,
    /// An annotated tag.
    Tag,
}

impl ObjectType {
    /// The keyword used in the object header and by `cat-file -t`.
    pub const fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Commit => "commit",
            ObjectType::Tag => "tag",
        }
    }

    /// Parse a header keyword.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "blob" => Ok(ObjectType::Blob),
            "tree" => Ok(ObjectType::Tree),
            "commit" => Ok(ObjectType::Commit),
            "tag" => Ok(ObjectType::Tag),
            other => Err(ForgeError::UnknownType(other.to_string())),
        }
    }
}

impl std::fmt::Display for ObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maximum size we will decompress for a single loose object (a fail-closed
/// guard against decompression bombs — see PLAN "Security contract"). 512 MiB
/// is far above any realistic loose object.
pub const MAX_OBJECT_SIZE: u64 = 512 * 1024 * 1024;

/// Build the loose-object bytes for a payload and return `(oid, full_bytes)`.
///
/// `full_bytes` is the *uncompressed* `"<type> <len>\0<payload>"` framing whose
/// SHA-1 is the object id. This is the single source of truth for hashing.
pub fn hash_content(obj_type: ObjectType, payload: &[u8]) -> (ObjectId, Vec<u8>) {
    let header = format!("{} {}\0", obj_type.as_str(), payload.len());
    let mut full = Vec::with_capacity(header.len() + payload.len());
    full.extend_from_slice(header.as_bytes());
    full.extend_from_slice(payload);

    let mut hasher = Sha1::new();
    hasher.update(&full);
    let digest = hasher.finalize();

    let mut bytes = [0u8; OID_LEN];
    bytes.copy_from_slice(&digest);
    (ObjectId::from_bytes(bytes), full)
}

/// Split an uncompressed loose object into its type and payload.
///
/// Fails closed if the header is missing, non-numeric, or if the declared
/// length disagrees with the actual payload length (a corruption signal).
pub fn parse_loose(full: &[u8]) -> Result<(ObjectType, &[u8])> {
    let nul = full
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| ForgeError::malformed("loose", "no NUL terminating the header"))?;
    let header = &full[..nul];

    let space = header
        .iter()
        .position(|&b| b == b' ')
        .ok_or_else(|| ForgeError::malformed("loose", "no space in header"))?;
    let type_str = std::str::from_utf8(&header[..space])
        .map_err(|_| ForgeError::malformed("loose", "type is not ASCII"))?;
    let len_str = std::str::from_utf8(&header[space + 1..])
        .map_err(|_| ForgeError::malformed("loose", "length is not ASCII"))?;

    let obj_type = ObjectType::parse(type_str)?;
    let declared: usize = len_str
        .parse()
        .map_err(|_| ForgeError::malformed("loose", "length is not a number"))?;

    let payload = &full[nul + 1..];
    if payload.len() != declared {
        return Err(ForgeError::malformed(
            "loose",
            format!(
                "declared length {declared} but payload is {} bytes",
                payload.len()
            ),
        ));
    }
    Ok((obj_type, payload))
}

/// A fully parsed Git object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    /// A blob.
    Blob(Blob),
    /// A tree.
    Tree(Tree),
    /// A commit.
    Commit(Commit),
    /// An annotated tag.
    Tag(Tag),
}

impl Object {
    /// Parse a typed object from its payload.
    pub fn parse(obj_type: ObjectType, payload: &[u8]) -> Result<Self> {
        Ok(match obj_type {
            ObjectType::Blob => Object::Blob(Blob::new(payload.to_vec())),
            ObjectType::Tree => Object::Tree(Tree::parse(payload)?),
            ObjectType::Commit => Object::Commit(Commit::parse(payload)?),
            ObjectType::Tag => Object::Tag(Tag::parse(payload)?),
        })
    }

    /// This object's type.
    pub fn object_type(&self) -> ObjectType {
        match self {
            Object::Blob(_) => ObjectType::Blob,
            Object::Tree(_) => ObjectType::Tree,
            Object::Commit(_) => ObjectType::Commit,
            Object::Tag(_) => ObjectType::Tag,
        }
    }

    /// The canonical payload bytes (round-trips Git's serialization).
    pub fn serialize_payload(&self) -> Vec<u8> {
        match self {
            Object::Blob(b) => b.data.clone(),
            Object::Tree(t) => t.serialize(),
            Object::Commit(c) => c.serialize(),
            Object::Tag(t) => t.serialize(),
        }
    }

    /// The object id this object hashes to.
    pub fn id(&self) -> ObjectId {
        hash_content(self.object_type(), &self.serialize_payload()).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blob_matches_git_constant() {
        // Every Git repo agrees on the empty blob oid.
        let (id, _) = hash_content(ObjectType::Blob, b"");
        assert_eq!(id.to_hex(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn hello_blob_matches_known_git_oid() {
        // `printf 'hello\n' | git hash-object --stdin` -> this oid.
        let (id, _) = hash_content(ObjectType::Blob, b"hello\n");
        assert_eq!(id.to_hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn loose_framing_roundtrips() {
        let (_, full) = hash_content(ObjectType::Blob, b"payload");
        let (ty, payload) = parse_loose(&full).unwrap();
        assert_eq!(ty, ObjectType::Blob);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn rejects_length_mismatch() {
        let mut full = b"blob 99\0short".to_vec();
        assert!(parse_loose(&full).is_err());
        full = b"blob\0nolen".to_vec();
        assert!(parse_loose(&full).is_err());
    }

    #[test]
    fn rejects_unknown_type() {
        assert!(parse_loose(b"widget 3\0abc").is_err());
    }
}

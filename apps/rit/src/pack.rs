//! Packed-object reads, adopted from **gitoxide** (the `gix-*` crates).
//!
//! ## Why this module exists (the OSS-fork strategy)
//!
//! Packfiles + delta resolution are the single biggest from-scratch cost in a
//! Git implementation (see `ARCHITECTURE.md` §7: the `rit-pack` crate is
//! estimated at ~12,000 LOC — the largest line item). Rather than re-implement
//! the `.pack`/`.idx` format, variable-length entry headers, the `OFS_DELTA` /
//! `REF_DELTA` copy-and-insert instruction stream, delta-chain resolution, and
//! the fan-out index binary search, `rit` **adopts gitoxide**: this module is a
//! thin adapter over [`gix_odb`], whose [`gix_pack`] engine already implements
//! all of the above, correctness-tested against real Git.
//!
//! ## Division of labour
//!
//! `rit` keeps ownership of the **loose** object path — its own hand-written,
//! interop-verified reader/writer in [`crate::odb`]. gitoxide is consulted only
//! as the **packed** path: when a loose object is absent, [`PackStore`] resolves
//! it out of `.git/objects/pack/*.pack` (following delta chains across the pack,
//! including bases stored as other packed or loose objects). Callers see one
//! unified interface via [`crate::odb::Odb`] and [`crate::repo::Repository`].
//!
//! ## Fail-closed
//!
//! Every gitoxide error is mapped to a typed [`ForgeError`] — never a panic. And
//! because a packfile is adversarial input just like a loose object, the bytes
//! gitoxide returns are **re-hashed** and checked against the requested object
//! id before being trusted, exactly mirroring the loose path's verification.

use crate::error::{ForgeError, Result};
use crate::object::{self, ObjectType};
use crate::oid::ObjectId;
use gix_object::{Exists, Find};
use std::path::Path;

/// A read-only view over a repository's packfiles, backed by gitoxide's
/// `gix-odb` store (which drives the `gix-pack` decoder).
///
/// Opening is cheap: gitoxide loads pack indices lazily on first access, so a
/// repository with no packs (or an empty object database) constructs a
/// `PackStore` that simply never finds anything.
pub struct PackStore {
    /// The gitoxide object-database handle rooted at `.git/objects`.
    handle: gix_odb::Handle,
}

impl PackStore {
    /// Open the gitoxide-backed store rooted at a `.git/objects` directory.
    pub fn open(objects_dir: &Path) -> Result<Self> {
        let handle = gix_odb::at(objects_dir)
            .map_err(|e| ForgeError::Pack(format!("opening object store: {e}")))?;
        Ok(Self { handle })
    }

    /// Convert a `rit` [`ObjectId`] into a gitoxide id (both are 20-byte SHA-1).
    fn to_gix(oid: &ObjectId) -> gix_hash::ObjectId {
        gix_hash::ObjectId::from(*oid.as_bytes())
    }

    /// Whether the object is resolvable through gitoxide (packed, or loose as a
    /// safety net). Consulted only after `rit`'s own loose check misses.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        let gid = Self::to_gix(oid);
        self.handle.exists(gid.as_ref())
    }

    /// Read an object's type and canonical, fully-inflated payload from a pack.
    ///
    /// Returns [`ForgeError::ObjectNotFound`] when the object is in no pack, a
    /// [`ForgeError::Pack`] on a decode failure (corrupt pack / broken delta
    /// chain), and [`ForgeError::HashMismatch`] if the decoded bytes do not hash
    /// back to `oid` (fail-closed integrity check).
    pub fn read_raw(&self, oid: &ObjectId) -> Result<(ObjectType, Vec<u8>)> {
        let gid = Self::to_gix(oid);
        let mut buf = Vec::new();
        let data = self
            .handle
            .try_find(gid.as_ref(), &mut buf)
            .map_err(|e| ForgeError::Pack(format!("decoding {oid}: {e}")))?
            .ok_or_else(|| ForgeError::ObjectNotFound(oid.to_hex()))?;

        let obj_type = map_kind(data.kind);
        let payload = data.data.to_vec();

        // Re-verify: the decompressed, delta-resolved bytes must hash back to the
        // requested oid — the same trust boundary the loose path enforces.
        let (computed, _) = object::hash_content(obj_type, &payload);
        if &computed != oid {
            return Err(ForgeError::HashMismatch {
                looked_up: oid.to_hex(),
                computed: computed.to_hex(),
            });
        }
        Ok((obj_type, payload))
    }

    /// Resolve an abbreviated hex prefix against packed objects.
    ///
    /// Returns `Ok(None)` when no packed object matches, `Ok(Some(id))` for a
    /// unique match, and [`ForgeError::AmbiguousPrefix`] when more than one
    /// packed object shares the prefix.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<Option<ObjectId>> {
        let gprefix = gix_hash::Prefix::from_hex(prefix)
            .map_err(|e| ForgeError::Pack(format!("invalid prefix {prefix:?}: {e}")))?;
        match self
            .handle
            .lookup_prefix(gprefix, None)
            .map_err(|e| ForgeError::Pack(format!("prefix lookup {prefix:?}: {e}")))?
        {
            None => Ok(None),
            Some(Err(())) => Err(ForgeError::AmbiguousPrefix(prefix.to_string())),
            Some(Ok(id)) => {
                let raw: [u8; 20] = id
                    .as_bytes()
                    .try_into()
                    .map_err(|_| ForgeError::Pack("unexpected hash length from pack".into()))?;
                Ok(Some(ObjectId::from_bytes(raw)))
            }
        }
    }
}

/// Map a gitoxide object kind onto `rit`'s [`ObjectType`].
pub(crate) fn map_kind(kind: gix_object::Kind) -> ObjectType {
    match kind {
        gix_object::Kind::Blob => ObjectType::Blob,
        gix_object::Kind::Tree => ObjectType::Tree,
        gix_object::Kind::Commit => ObjectType::Commit,
        gix_object::Kind::Tag => ObjectType::Tag,
    }
}

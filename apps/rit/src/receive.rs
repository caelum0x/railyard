//! Quarantined receive transaction for decoded Git objects.
//!
//! Network and pack negotiation are deliberately outside this module. Once an
//! open-source pack decoder yields typed payloads, this is the safety boundary:
//! isolate them, verify declared identities, fsck the complete overlay graph,
//! enforce fast-forward policy, publish immutable objects, then move one ref by
//! compare-and-swap. A failed transaction never moves the ref.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

use crate::error::{ForgeError, Result};
use crate::fsck::{check_connectivity, is_ancestor, ConnectivityLimits, ObjectReader};
use crate::object::{Object, ObjectType, MAX_OBJECT_SIZE};
use crate::odb::Odb;
use crate::oid::ObjectId;
use crate::refs::validate_refname;
use crate::Repository;

static NEXT_QUARANTINE: AtomicU64 = AtomicU64::new(0);

/// Limits applied to the decoded object batch before graph traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiveLimits {
    pub max_objects: usize,
    pub max_pack_bytes: u64,
    pub max_total_bytes: u64,
    pub connectivity: ConnectivityLimits,
}

impl Default for ReceiveLimits {
    fn default() -> Self {
        Self {
            max_objects: 250_000,
            max_pack_bytes: 1024 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            connectivity: ConnectivityLimits::default(),
        }
    }
}

/// One decoded object supplied by the pack/protocol layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveObject {
    pub declared_oid: ObjectId,
    pub object_type: ObjectType,
    pub payload: Vec<u8>,
}

/// One ref update. This first safe receive slice deliberately owns exactly one
/// ref per transaction; multi-ref atomic transactions remain a later wire-layer
/// capability rather than pretending sequential renames are globally atomic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    pub expected_old: Option<ObjectId>,
    pub new: ObjectId,
    pub allow_non_fast_forward: bool,
}

/// Successful publication summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveOutcome {
    pub ref_name: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
    pub published_objects: usize,
}

/// Isolated decoded-object transaction tied to one target repository.
pub struct ReceiveTransaction<'repo> {
    repository: &'repo Repository,
    quarantine_root: PathBuf,
    quarantine_odb: Odb,
    limits: ReceiveLimits,
    staged: BTreeSet<ObjectId>,
    total_bytes: u64,
    pack_import_failed: Option<String>,
}

impl<'repo> ReceiveTransaction<'repo> {
    /// Allocate a repository-owned quarantine directory. The caller never
    /// supplies its name, preventing path traversal through correlation ids.
    pub fn begin(repository: &'repo Repository, limits: ReceiveLimits) -> Result<Self> {
        let parent = repository.git_dir().join("rit-quarantine");
        std::fs::create_dir_all(&parent)?;
        reject_symlink(&parent)?;
        let quarantine_root = allocate_quarantine(&parent)?;
        std::fs::create_dir(quarantine_root.join("objects"))?;
        let quarantine_odb = Odb::new(&quarantine_root);
        Ok(Self {
            repository,
            quarantine_root,
            quarantine_odb,
            limits,
            staged: BTreeSet::new(),
            total_bytes: 0,
            pack_import_failed: None,
        })
    }

    /// Verify and isolate one decoded object. Duplicate identical objects are
    /// idempotent and do not consume the batch count/byte budget twice.
    pub fn stage(&mut self, object: ReceiveObject) -> Result<()> {
        let payload_len = u64::try_from(object.payload.len())
            .map_err(|_| ForgeError::ResourceLimit("object length overflow".into()))?;
        if payload_len > MAX_OBJECT_SIZE {
            return Err(ForgeError::ResourceLimit(format!(
                "received object exceeds {MAX_OBJECT_SIZE} bytes"
            )));
        }
        let computed = crate::hash_content(object.object_type, &object.payload).0;
        if computed != object.declared_oid {
            return Err(ForgeError::HashMismatch {
                looked_up: object.declared_oid.to_hex(),
                computed: computed.to_hex(),
            });
        }
        if self.staged.contains(&computed) {
            return Ok(());
        }
        if self.staged.len() == self.limits.max_objects {
            return Err(ForgeError::ResourceLimit(format!(
                "receive exceeds {} objects",
                self.limits.max_objects
            )));
        }
        let next_total = self
            .total_bytes
            .checked_add(payload_len)
            .ok_or_else(|| ForgeError::ResourceLimit("receive byte count overflow".into()))?;
        if next_total > self.limits.max_total_bytes {
            return Err(ForgeError::ResourceLimit(format!(
                "receive exceeds {} decoded bytes",
                self.limits.max_total_bytes
            )));
        }
        let written = self
            .quarantine_odb
            .write(object.object_type, &object.payload)?;
        if written != object.declared_oid {
            return Err(ForgeError::HashMismatch {
                looked_up: object.declared_oid.to_hex(),
                computed: written.to_hex(),
            });
        }
        self.staged.insert(written);
        self.total_bytes = next_total;
        Ok(())
    }

    /// Validate and decode one complete Git pack with gitoxide, then
    /// stage every verified object into this transaction's loose quarantine.
    ///
    /// The import is deliberately two-phase. gitoxide first writes the raw pack
    /// and generated index beneath a transaction-owned scratch directory while
    /// checking its checksum and resolving in-pack deltas. Decoded objects are
    /// re-hashed into a second scratch ODB. Only after the entire pack and all
    /// decoded object identities pass their limits are they copied into the
    /// publishable quarantine. A malformed pack therefore cannot make a
    /// partially decoded graph eligible for [`finish`](Self::finish).
    ///
    /// Thin-pack REF_DELTA bases are resolved only through [`VerifiedBaseLookup`],
    /// which reads from the transaction quarantine or target ODB and therefore
    /// retains their normal object-id verification. gitoxide expands the thin
    /// input into a self-contained scratch pack before any object is staged.
    pub fn stage_pack(&mut self, pack: &[u8]) -> Result<usize> {
        let staged_before = self.staged.len();
        let header = verify_pack_envelope(pack, self.limits)?;
        let scratch_root = allocate_quarantine(&self.quarantine_root)?;
        let _scratch = ScratchDirectory(scratch_root.clone());
        let pack_dir = scratch_root.join("pack");
        let decoded_root = scratch_root.join("decoded");
        std::fs::create_dir(&pack_dir)?;
        std::fs::create_dir(&decoded_root)?;
        std::fs::create_dir(decoded_root.join("objects"))?;

        let mut input = Cursor::new(pack);
        let mut progress = gix_features::progress::Discard;
        let interrupted = AtomicBool::new(false);
        let outcome = gix_pack::Bundle::write_to_directory(
            &mut input,
            Some(&pack_dir),
            &mut progress,
            &interrupted,
            Some(VerifiedBaseLookup {
                quarantine: &self.quarantine_odb,
                target: self.repository.odb(),
            }),
            gix_pack::bundle::write::Options {
                thread_limit: Some(1),
                iteration_mode: gix_pack::data::input::Mode::Verify,
                index_version: gix_pack::index::Version::default(),
                object_hash: gix_hash::Kind::Sha1,
            },
        )
        .map_err(|error| ForgeError::Pack(format!("validating received pack: {error}")))?;

        let indexed_objects = usize::try_from(outcome.index.num_objects)
            .map_err(|_| ForgeError::ResourceLimit("pack object count overflow".into()))?;
        if indexed_objects < header.object_count {
            return Err(ForgeError::Pack(format!(
                "pack header declares {} objects but expanded index contains only {indexed_objects}",
                header.object_count
            )));
        }
        if indexed_objects == 0 {
            return Ok(0);
        }

        let bundle = outcome
            .to_bundle()
            .ok_or_else(|| ForgeError::Pack("non-empty pack did not produce an index".into()))?
            .map_err(|error| ForgeError::Pack(format!("opening verified pack: {error}")))?;
        let decoded_odb = Odb::new(&decoded_root);
        let mut decoded = Vec::with_capacity(indexed_objects);
        let mut unique = BTreeSet::new();
        let mut new_decoded_bytes = 0u64;
        let mut buffer = Vec::new();
        let mut inflate = gix_features::zlib::Inflate::default();
        let mut cache = gix_pack::cache::Never;

        for index in 0..outcome.index.num_objects {
            buffer.clear();
            let expected = bundle.index.oid_at_index(index).to_owned();
            let (data, _) = bundle
                .get_object_by_index(index, &mut buffer, &mut inflate, &mut cache)
                .map_err(|error| ForgeError::Pack(format!("decoding received object: {error}")))?;
            let object_type = crate::pack::map_kind(data.kind);
            let payload_len = u64::try_from(data.data.len())
                .map_err(|_| ForgeError::ResourceLimit("decoded object length overflow".into()))?;
            if payload_len > MAX_OBJECT_SIZE {
                return Err(ForgeError::ResourceLimit(format!(
                    "received object exceeds {MAX_OBJECT_SIZE} bytes"
                )));
            }
            let expected = gix_oid_to_rit(&expected)?;
            let computed = crate::hash_content(object_type, data.data).0;
            if computed != expected {
                return Err(ForgeError::HashMismatch {
                    looked_up: expected.to_hex(),
                    computed: computed.to_hex(),
                });
            }
            let written = decoded_odb.write(object_type, data.data)?;
            if written != expected {
                return Err(ForgeError::HashMismatch {
                    looked_up: expected.to_hex(),
                    computed: written.to_hex(),
                });
            }
            if unique.insert(expected)
                && !self.staged.contains(&expected)
                && !self.repository.odb().contains(&expected)
            {
                new_decoded_bytes =
                    new_decoded_bytes.checked_add(payload_len).ok_or_else(|| {
                        ForgeError::ResourceLimit("receive byte count overflow".into())
                    })?;
            }
            decoded.push((expected, object_type));
        }

        let new_objects = unique
            .iter()
            .filter(|oid| !self.staged.contains(oid) && !self.repository.odb().contains(oid))
            .count();
        if self.staged.len().saturating_add(new_objects) > self.limits.max_objects {
            return Err(ForgeError::ResourceLimit(format!(
                "receive exceeds {} objects",
                self.limits.max_objects
            )));
        }
        let combined_bytes = self
            .total_bytes
            .checked_add(new_decoded_bytes)
            .ok_or_else(|| ForgeError::ResourceLimit("receive byte count overflow".into()))?;
        if combined_bytes > self.limits.max_total_bytes {
            return Err(ForgeError::ResourceLimit(format!(
                "receive exceeds {} decoded bytes",
                self.limits.max_total_bytes
            )));
        }

        for (oid, object_type) in decoded {
            if self.staged.contains(&oid) || self.repository.odb().contains(&oid) {
                continue;
            }
            let (_, payload) = decoded_odb.read_raw(&oid)?;
            if let Err(error) = self.stage(ReceiveObject {
                declared_oid: oid,
                object_type,
                payload,
            }) {
                self.pack_import_failed = Some(error.to_string());
                return Err(error);
            }
        }
        Ok(self.staged.len() - staged_before)
    }

    /// Validate and publish the complete graph, then atomically compare-and-swap
    /// its single destination ref. Object publication precedes the ref update;
    /// if a concurrent writer wins, only unreachable immutable objects remain.
    pub fn finish(self, update: RefUpdate) -> Result<ReceiveOutcome> {
        if let Some(reason) = &self.pack_import_failed {
            return Err(ForgeError::ReceiveRejected(format!(
                "an earlier pack import failed after staging began: {reason}"
            )));
        }
        validate_refname(&update.name)?;
        let reader = OverlayReader {
            quarantine: &self.quarantine_odb,
            target: self.repository.odb(),
            staged: &self.staged,
        };
        let report = check_connectivity(&reader, &[update.new], self.limits.connectivity)?;
        if !self.staged.iter().all(|oid| report.reachable.contains(oid)) {
            return Err(ForgeError::ReceiveRejected(
                "quarantine contains objects disconnected from the proposed ref tip".into(),
            ));
        }

        if update.name.starts_with("refs/heads/") {
            let Object::Commit(_) = reader.read_object(&update.new)? else {
                return Err(ForgeError::Connectivity(format!(
                    "branch tip {} is not a commit",
                    update.new
                )));
            };
            if let Some(old) = update.expected_old {
                if !update.allow_non_fast_forward
                    && !is_ancestor(&reader, old, update.new, self.limits.connectivity)?
                {
                    return Err(ForgeError::NonFastForward {
                        name: update.name,
                        old: old.to_hex(),
                        new: update.new.to_hex(),
                    });
                }
            }
        }

        for oid in &self.staged {
            let (object_type, payload) = self.quarantine_odb.read_raw(oid)?;
            let published = self.repository.odb().write(object_type, &payload)?;
            if published != *oid {
                return Err(ForgeError::HashMismatch {
                    looked_up: oid.to_hex(),
                    computed: published.to_hex(),
                });
            }
        }
        self.repository
            .refs()
            .compare_and_swap(&update.name, update.expected_old, update.new)?;
        Ok(ReceiveOutcome {
            ref_name: update.name,
            old: update.expected_old,
            new: update.new,
            published_objects: self.staged.len(),
        })
    }

    pub fn staged_objects(&self) -> usize {
        self.staged.len()
    }

    pub fn decoded_bytes(&self) -> u64 {
        self.total_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PackHeader {
    object_count: usize,
}

fn verify_pack_envelope(pack: &[u8], limits: ReceiveLimits) -> Result<PackHeader> {
    const HEADER_LEN: usize = 12;
    const TRAILER_LEN: usize = 20;
    if pack.len() < HEADER_LEN + TRAILER_LEN {
        return Err(ForgeError::Pack("received pack is truncated".into()));
    }
    let pack_len = u64::try_from(pack.len())
        .map_err(|_| ForgeError::ResourceLimit("pack length overflow".into()))?;
    if pack_len > limits.max_pack_bytes {
        return Err(ForgeError::ResourceLimit(format!(
            "receive pack exceeds {} bytes",
            limits.max_pack_bytes
        )));
    }
    if &pack[..4] != b"PACK" {
        return Err(ForgeError::Pack("received stream is not a Git pack".into()));
    }
    let version = u32::from_be_bytes(pack[4..8].try_into().expect("fixed-width pack version"));
    if !matches!(version, 2 | 3) {
        return Err(ForgeError::Pack(format!(
            "unsupported pack version {version}"
        )));
    }
    let object_count = usize::try_from(u32::from_be_bytes(
        pack[8..12].try_into().expect("fixed-width pack count"),
    ))
    .map_err(|_| ForgeError::ResourceLimit("pack object count overflow".into()))?;
    if object_count > limits.max_objects {
        return Err(ForgeError::ResourceLimit(format!(
            "pack declares {object_count} objects, limit is {}",
            limits.max_objects
        )));
    }

    let trailer_at = pack.len() - TRAILER_LEN;
    let computed = Sha1::digest(&pack[..trailer_at]);
    if computed.as_slice() != &pack[trailer_at..] {
        return Err(ForgeError::Pack("received pack checksum mismatch".into()));
    }
    Ok(PackHeader { object_count })
}

fn gix_oid_to_rit(oid: &gix_hash::oid) -> Result<ObjectId> {
    let raw: [u8; 20] = oid
        .as_bytes()
        .try_into()
        .map_err(|_| ForgeError::Pack("unexpected non-SHA-1 object id in pack index".into()))?;
    Ok(ObjectId::from_bytes(raw))
}

struct ScratchDirectory(PathBuf);

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// gitoxide callback used only while expanding thin-pack REF_DELTA bases. Both
/// backing ODBs verify loose/packed object identities before returning bytes,
/// so an attacker cannot substitute a base merely by controlling a filename.
struct VerifiedBaseLookup<'a> {
    quarantine: &'a Odb,
    target: &'a Odb,
}

impl gix_object::Find for VerifiedBaseLookup<'_> {
    fn try_find<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> std::result::Result<Option<gix_object::Data<'a>>, gix_object::find::Error> {
        let oid = gix_oid_to_rit(id).map_err(boxed_find_error)?;
        let object = match self.quarantine.read_raw(&oid) {
            Ok(object) => Some(object),
            Err(ForgeError::ObjectNotFound(_)) => match self.target.read_raw(&oid) {
                Ok(object) => Some(object),
                Err(ForgeError::ObjectNotFound(_)) => None,
                Err(error) => return Err(boxed_find_error(error)),
            },
            Err(error) => return Err(boxed_find_error(error)),
        };
        let Some((object_type, payload)) = object else {
            return Ok(None);
        };
        buffer.clear();
        buffer.extend_from_slice(&payload);
        Ok(Some(gix_object::Data {
            kind: rit_kind_to_gix(object_type),
            data: buffer.as_slice(),
        }))
    }
}

fn rit_kind_to_gix(object_type: ObjectType) -> gix_object::Kind {
    match object_type {
        ObjectType::Blob => gix_object::Kind::Blob,
        ObjectType::Tree => gix_object::Kind::Tree,
        ObjectType::Commit => gix_object::Kind::Commit,
        ObjectType::Tag => gix_object::Kind::Tag,
    }
}

fn boxed_find_error(error: ForgeError) -> gix_object::find::Error {
    Box::new(error)
}

impl Drop for ReceiveTransaction<'_> {
    fn drop(&mut self) {
        // This path is allocated internally beneath `.git/rit-quarantine` and
        // never accepts user components. Best-effort cleanup cannot affect the
        // target object database or refs.
        let _ = std::fs::remove_dir_all(&self.quarantine_root);
    }
}

struct OverlayReader<'a> {
    quarantine: &'a Odb,
    target: &'a Odb,
    staged: &'a BTreeSet<ObjectId>,
}

impl ObjectReader for OverlayReader<'_> {
    fn read_object(&self, oid: &ObjectId) -> Result<Object> {
        if self.staged.contains(oid) {
            self.quarantine.read(oid)
        } else {
            self.target.read(oid)
        }
    }
}

fn allocate_quarantine(parent: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    for _ in 0..128 {
        let sequence = NEXT_QUARANTINE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            "incoming-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(ForgeError::ReceiveRejected(
        "could not allocate quarantine directory".into(),
    ))
}

fn reject_symlink(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(ForgeError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::Commit;
    use crate::signature::Signature;
    use crate::tree::Tree;

    fn bare_repository() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        std::fs::create_dir_all(dir.path().join("refs/heads")).unwrap();
        let repo = Repository::open_git_dir(dir.path()).unwrap();
        (dir, repo)
    }

    fn commit_objects(message: &[u8], parent: Option<ObjectId>) -> Vec<ReceiveObject> {
        let tree = Tree::default().serialize();
        let tree_oid = crate::hash_content(ObjectType::Tree, &tree).0;
        let signature =
            Signature::parse("Rit <rit@example.com> 1700000000 +0000", "commit").unwrap();
        let commit = Commit {
            tree: tree_oid,
            parents: parent.into_iter().collect(),
            author: signature.clone(),
            committer: signature,
            extra_headers: Vec::new(),
            message: message.to_vec(),
        }
        .serialize();
        let commit_oid = crate::hash_content(ObjectType::Commit, &commit).0;
        vec![
            ReceiveObject {
                declared_oid: tree_oid,
                object_type: ObjectType::Tree,
                payload: tree,
            },
            ReceiveObject {
                declared_oid: commit_oid,
                object_type: ObjectType::Commit,
                payload: commit,
            },
        ]
    }

    fn empty_pack() -> Vec<u8> {
        let mut pack = Vec::from(&b"PACK\0\0\0\x02\0\0\0\0"[..]);
        let checksum = Sha1::digest(&pack);
        pack.extend_from_slice(&checksum);
        pack
    }

    #[test]
    fn empty_raw_pack_is_valid_and_stages_nothing() {
        let (_dir, repo) = bare_repository();
        let mut receive = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        assert_eq!(receive.stage_pack(&empty_pack()).unwrap(), 0);
        assert_eq!(receive.staged_objects(), 0);
        assert_eq!(receive.decoded_bytes(), 0);
    }

    #[test]
    fn pack_envelope_rejects_bad_magic_version_count_checksum_and_size() {
        let valid = empty_pack();
        assert_eq!(
            verify_pack_envelope(&valid, ReceiveLimits::default())
                .unwrap()
                .object_count,
            0
        );

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'B';
        assert!(matches!(
            verify_pack_envelope(&bad_magic, ReceiveLimits::default()),
            Err(ForgeError::Pack(_))
        ));

        let mut bad_version = valid.clone();
        bad_version[7] = 4;
        assert!(matches!(
            verify_pack_envelope(&bad_version, ReceiveLimits::default()),
            Err(ForgeError::Pack(_))
        ));

        let mut excessive_count = valid.clone();
        excessive_count[8..12].copy_from_slice(&2u32.to_be_bytes());
        let limits = ReceiveLimits {
            max_objects: 1,
            ..ReceiveLimits::default()
        };
        assert!(matches!(
            verify_pack_envelope(&excessive_count, limits),
            Err(ForgeError::ResourceLimit(_))
        ));

        let mut bad_checksum = valid.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        assert!(matches!(
            verify_pack_envelope(&bad_checksum, ReceiveLimits::default()),
            Err(ForgeError::Pack(_))
        ));

        let limits = ReceiveLimits {
            max_pack_bytes: (valid.len() - 1) as u64,
            ..ReceiveLimits::default()
        };
        assert!(matches!(
            verify_pack_envelope(&valid, limits),
            Err(ForgeError::ResourceLimit(_))
        ));
    }

    #[test]
    fn publishes_connected_objects_then_moves_one_ref() {
        let (_dir, repo) = bare_repository();
        let objects = commit_objects(b"root\n", None);
        let tip = objects.last().unwrap().declared_oid;
        let mut receive = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        for object in objects {
            receive.stage(object).unwrap();
        }
        let outcome = receive
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: None,
                new: tip,
                allow_non_fast_forward: false,
            })
            .unwrap();
        assert_eq!(outcome.published_objects, 2);
        assert_eq!(repo.refs().resolve("refs/heads/main").unwrap(), tip);
        assert!(repo.odb().contains(&tip));
    }

    #[test]
    fn hash_mismatch_and_missing_connectivity_never_move_ref() {
        let (_dir, repo) = bare_repository();
        let objects = commit_objects(b"root\n", None);
        let tip = objects.last().unwrap().declared_oid;
        let mut receive = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        let mut corrupt = objects.last().unwrap().clone();
        corrupt.payload.push(b'!');
        assert!(matches!(
            receive.stage(corrupt),
            Err(ForgeError::HashMismatch { .. })
        ));
        assert!(repo.refs().resolve("refs/heads/main").is_err());

        receive.stage(objects.last().unwrap().clone()).unwrap();
        assert!(receive
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: None,
                new: tip,
                allow_non_fast_forward: false,
            })
            .is_err());
        assert!(repo.refs().resolve("refs/heads/main").is_err());
        assert!(!repo.odb().contains(&tip));
    }

    #[test]
    fn rejects_disconnected_objects_and_non_fast_forward() {
        let (_dir, repo) = bare_repository();
        let root_objects = commit_objects(b"root\n", None);
        let root = root_objects.last().unwrap().declared_oid;
        let mut first = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        for object in root_objects {
            first.stage(object).unwrap();
        }
        first
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: None,
                new: root,
                allow_non_fast_forward: false,
            })
            .unwrap();

        let replacement = commit_objects(b"replacement\n", None);
        let replacement_tip = replacement.last().unwrap().declared_oid;
        let mut receive = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        for object in replacement {
            receive.stage(object).unwrap();
        }
        let extra_payload = b"disconnected".to_vec();
        let extra_oid = crate::hash_content(ObjectType::Blob, &extra_payload).0;
        receive
            .stage(ReceiveObject {
                declared_oid: extra_oid,
                object_type: ObjectType::Blob,
                payload: extra_payload,
            })
            .unwrap();
        assert!(receive
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: Some(root),
                new: replacement_tip,
                allow_non_fast_forward: false,
            })
            .is_err());
        assert_eq!(repo.refs().resolve("refs/heads/main").unwrap(), root);

        let mut receive = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        for object in commit_objects(b"replacement\n", None) {
            receive.stage(object).unwrap();
        }
        assert!(matches!(
            receive.finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: Some(root),
                new: replacement_tip,
                allow_non_fast_forward: false,
            }),
            Err(ForgeError::NonFastForward { .. })
        ));
        assert_eq!(repo.refs().resolve("refs/heads/main").unwrap(), root);
    }

    #[test]
    fn accepts_a_fast_forward_whose_parent_is_already_published() {
        let (_dir, repo) = bare_repository();
        let root_objects = commit_objects(b"root\n", None);
        let root = root_objects.last().unwrap().declared_oid;
        let mut first = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        for object in root_objects {
            first.stage(object).unwrap();
        }
        first
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: None,
                new: root,
                allow_non_fast_forward: false,
            })
            .unwrap();

        let next_objects = commit_objects(b"next\n", Some(root));
        let next = next_objects.last().unwrap().declared_oid;
        let mut second = ReceiveTransaction::begin(&repo, ReceiveLimits::default()).unwrap();
        // The empty tree is already in the target ODB. Staging only the new
        // commit proves connectivity can cross the quarantine overlay boundary.
        second.stage(next_objects.last().unwrap().clone()).unwrap();
        second
            .finish(RefUpdate {
                name: "refs/heads/main".into(),
                expected_old: Some(root),
                new: next,
                allow_non_fast_forward: false,
            })
            .unwrap();
        assert_eq!(repo.refs().resolve("refs/heads/main").unwrap(), next);
    }
}

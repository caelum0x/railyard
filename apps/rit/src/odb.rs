//! The loose object database: read, write, and verify objects stored under
//! `.git/objects/xx/<38-hex>` in zlib-compressed loose format.
//!
//! Reads **always verify** that the decompressed bytes hash back to the oid
//! they were requested under — corruption is surfaced as an error, never
//! silently returned (PLAN: "inspect ... without corrupting data").

use crate::error::{ForgeError, Result};
use crate::object::{self, Object, ObjectType, MAX_OBJECT_SIZE};
use crate::oid::ObjectId;
use crate::pack::PackStore;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The object database rooted at a `.git/objects` directory.
///
/// `rit` owns the **loose** path (write + verified read of `objects/xx/<rest>`),
/// while the **packed** path is delegated to gitoxide via an optional
/// [`PackStore`]. Reads try loose first (the `rit` code), then fall back to
/// packs (gitoxide), so every consumer sees one unified object interface.
pub struct Odb {
    objects_dir: PathBuf,
    /// Packed-object reader (gitoxide). `None` only if the store could not be
    /// opened at all; a missing/empty pack directory still yields a working
    /// store that simply never finds anything.
    pack: Option<PackStore>,
}

impl Odb {
    /// Open the object database inside a `.git` directory.
    pub fn new(git_dir: &Path) -> Self {
        let objects_dir = git_dir.join("objects");
        // Opening the gitoxide store is cheap (indices load lazily). If it fails
        // we simply have no packed fallback; the loose path still works, and any
        // genuinely corrupt pack surfaces as a typed error at read time.
        let pack = PackStore::open(&objects_dir).ok();
        Self { objects_dir, pack }
    }

    /// The on-disk path for an object (`objects/xx/<rest>`).
    pub fn loose_path(&self, oid: &ObjectId) -> PathBuf {
        self.objects_dir
            .join(oid.dir_prefix())
            .join(oid.file_suffix())
    }

    /// Whether an object exists, loose **or** packed.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.loose_path(oid).exists() || self.pack.as_ref().is_some_and(|p| p.contains(oid))
    }

    /// Write a payload as a loose object and return its oid.
    ///
    /// Idempotent: if the object already exists it is left untouched (its oid is
    /// content-addressed, so identical content is identical bytes). The write is
    /// atomic — bytes land in a temp file, are fsync'd, then renamed into place.
    pub fn write(&self, obj_type: ObjectType, payload: &[u8]) -> Result<ObjectId> {
        let (oid, full) = object::hash_content(obj_type, payload);
        let final_path = self.loose_path(&oid);
        if final_path.exists() {
            // Content addressing is only safe if an existing path really holds
            // the expected bytes. Never bless a corrupt or symlinked object.
            self.read_raw(&oid)?;
            return Ok(oid);
        }

        let dir = final_path
            .parent()
            .expect("loose path always has a parent directory");
        std::fs::create_dir_all(dir)?;
        reject_symlink(dir)?;

        let compressed = zlib_compress(&full)?;

        // Atomic publish: write to a unique temp file, flush + fsync, rename.
        let (tmp_path, mut file) = self.create_temp(dir)?;
        {
            file.write_all(&compressed)?;
            file.sync_all()?;
        }
        // `hard_link` is an atomic no-clobber publish on the same filesystem.
        // Concurrent identical writers converge; neither can replace an
        // existing (possibly corrupt) object behind the other's back.
        match std::fs::hard_link(&tmp_path, &final_path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp_path);
                sync_dir(dir)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp_path);
                self.read_raw(&oid)?;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error.into());
            }
        }
        Ok(oid)
    }

    /// Read an object's raw type and payload, verifying its SHA-1.
    ///
    /// Tries the loose store first (rit's own reader); on a loose miss, falls
    /// back to the packed store (gitoxide). Both paths verify the bytes hash
    /// back to `oid` before returning them.
    pub fn read_raw(&self, oid: &ObjectId) -> Result<(ObjectType, Vec<u8>)> {
        let path = self.loose_path(oid);
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(ForgeError::UnsafePath(path.display().to_string()));
            }
        }
        let compressed = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            // Not loose — try the packed path (gitoxide) if we have one.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return match &self.pack {
                    Some(pack) => pack.read_raw(oid),
                    None => Err(ForgeError::ObjectNotFound(oid.to_hex())),
                };
            }
            Err(error) => return Err(error.into()),
        };
        let full = zlib_decompress(&compressed)?;

        // Verify integrity before trusting the bytes.
        let computed = {
            use sha1::{Digest, Sha1};
            let mut h = Sha1::new();
            h.update(&full);
            let d = h.finalize();
            let mut b = [0u8; 20];
            b.copy_from_slice(&d);
            ObjectId::from_bytes(b)
        };
        if &computed != oid {
            return Err(ForgeError::HashMismatch {
                looked_up: oid.to_hex(),
                computed: computed.to_hex(),
            });
        }

        let (obj_type, payload) = object::parse_loose(&full)?;
        Ok((obj_type, payload.to_vec()))
    }

    /// Read and fully parse an object.
    pub fn read(&self, oid: &ObjectId) -> Result<Object> {
        let (obj_type, payload) = self.read_raw(oid)?;
        Object::parse(obj_type, &payload)
    }

    /// Read only an object's type (`cat-file -t`).
    pub fn object_type(&self, oid: &ObjectId) -> Result<ObjectType> {
        Ok(self.read_raw(oid)?.0)
    }

    /// Resolve an abbreviated hex prefix (>= 4 chars) to a unique full oid.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<ObjectId> {
        if prefix.len() < 4
            || prefix.len() > 40
            || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ForgeError::InvalidOid(format!(
                "prefix must be 4-40 hex chars: {prefix:?}"
            )));
        }
        let (dir_part, file_part) = prefix.split_at(2.min(prefix.len()));
        let subdir = self.objects_dir.join(dir_part);
        let mut matches: Vec<ObjectId> = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(&subdir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let full_hex = format!("{dir_part}{name}");
                if full_hex.starts_with(prefix) {
                    if let Ok(oid) = ObjectId::from_hex(&full_hex) {
                        matches.push(oid);
                    }
                }
            }
        }
        let _ = file_part;

        // Also consult packed objects (gitoxide). After `git repack`/`git gc`
        // the loose copies are gone, so packed prefix resolution keeps
        // abbreviated-oid lookups working on packed repositories.
        if let Some(pack) = &self.pack {
            match pack.resolve_prefix(prefix) {
                Ok(Some(id)) => {
                    if !matches.contains(&id) {
                        matches.push(id);
                    }
                }
                Ok(None) => {}
                // A prefix ambiguous within the packs is ambiguous overall.
                Err(ForgeError::AmbiguousPrefix(_)) => {
                    return Err(ForgeError::AmbiguousPrefix(prefix.to_string()))
                }
                Err(e) => return Err(e),
            }
        }

        match matches.len() {
            0 => Err(ForgeError::ObjectNotFound(prefix.to_string())),
            1 => Ok(matches[0]),
            _ => Err(ForgeError::AmbiguousPrefix(prefix.to_string())),
        }
    }

    /// Build a unique temporary file path inside `dir` (no extra deps).
    fn create_temp(&self, dir: &Path) -> Result<(PathBuf, std::fs::File)> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        for _ in 0..128 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".rit-tmp-{pid}-{nanos}-{sequence}"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ForgeError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique loose-object temporary file",
        )))
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(ForgeError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Compress bytes with zlib (the format Git uses for loose objects).
fn zlib_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder
        .finish()
        .map_err(|e| ForgeError::Decompress(format!("compression failed: {e}")))
}

/// Decompress a zlib stream, guarding against decompression bombs.
fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let decoder = ZlibDecoder::new(data);
    let mut limited = decoder.take(MAX_OBJECT_SIZE + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|e| ForgeError::Decompress(e.to_string()))?;
    if out.len() as u64 > MAX_OBJECT_SIZE {
        return Err(ForgeError::Decompress(format!(
            "object exceeds {MAX_OBJECT_SIZE} byte cap"
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Object;

    fn temp_git_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        dir
    }

    #[test]
    fn write_then_read_roundtrips_blob() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let oid = odb.write(ObjectType::Blob, b"hello odb\n").unwrap();
        assert!(odb.contains(&oid));
        match odb.read(&oid).unwrap() {
            Object::Blob(b) => assert_eq!(b.data, b"hello odb\n"),
            other => panic!("expected blob, got {other:?}"),
        }
    }

    #[test]
    fn write_is_idempotent() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let a = odb.write(ObjectType::Blob, b"same").unwrap();
        let b = odb.write(ObjectType::Blob, b"same").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn detects_corruption() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let oid = odb.write(ObjectType::Blob, b"trust me").unwrap();
        // Corrupt the stored bytes.
        let path = odb.loose_path(&oid);
        std::fs::write(&path, b"not even zlib").unwrap();
        assert!(odb.read_raw(&oid).is_err());
    }

    #[test]
    fn resolve_prefix_finds_unique() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let oid = odb.write(ObjectType::Blob, b"prefix me\n").unwrap();
        let short = &oid.to_hex()[..8];
        assert_eq!(odb.resolve_prefix(short).unwrap(), oid);
    }

    #[test]
    fn prefix_cannot_escape_objects_directory() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        assert!(matches!(
            odb.resolve_prefix("../../etc"),
            Err(ForgeError::InvalidOid(_))
        ));
    }

    #[test]
    fn write_does_not_bless_an_existing_corrupt_object() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let oid = odb.write(ObjectType::Blob, b"important").unwrap();
        std::fs::write(odb.loose_path(&oid), b"corrupt").unwrap();
        assert!(odb.write(ObjectType::Blob, b"important").is_err());
    }

    #[test]
    fn concurrent_writers_publish_one_verified_object() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = temp_git_dir();
        let root = Arc::new(dir.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(5));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let root = root.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                let odb = Odb::new(&root);
                barrier.wait();
                odb.write(ObjectType::Blob, b"same concurrent object")
            }));
        }
        barrier.wait();
        let ids: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            Odb::new(&root).read_raw(&ids[0]).unwrap().1,
            b"same concurrent object"
        );
    }

    #[test]
    fn orphaned_crash_temp_does_not_block_next_publish() {
        let dir = temp_git_dir();
        let odb = Odb::new(dir.path());
        let (oid, _) = object::hash_content(ObjectType::Blob, b"after crash");
        let shard = odb.loose_path(&oid).parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join(".rit-tmp-dead-writer"), b"partial zlib").unwrap();

        assert_eq!(odb.write(ObjectType::Blob, b"after crash").unwrap(), oid);
        assert_eq!(odb.read_raw(&oid).unwrap().1, b"after crash");
    }
}

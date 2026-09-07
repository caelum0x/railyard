//! Bounded reader for the on-disk Git index (`DIRC`) v2/v3.
//!
//! The checksum and every entry boundary are verified before paths are exposed.
//! Version 4 path compression is rejected explicitly until implemented rather
//! than being misparsed as v2.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::{ForgeError, ObjectId, Result};

const HEADER_LEN: usize = 12;
const CHECKSUM_LEN: usize = 20;
const ENTRY_FIXED_LEN: usize = 62;
const MAX_INDEX_ENTRIES: u32 = 1_000_000;
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub mode: u32,
    pub file_size: u32,
    pub oid: ObjectId,
    pub flags: u16,
    pub path: Vec<u8>,
}

impl IndexEntry {
    pub fn stage(&self) -> u8 {
        ((self.flags >> 12) & 0x3) as u8
    }

    pub fn path_str(&self) -> String {
        String::from_utf8_lossy(&self.path).into_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub version: u32,
    pub entries: Vec<IndexEntry>,
}

impl Index {
    pub fn read(git_dir: &Path) -> Result<Self> {
        let path = git_dir.join("index");
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_INDEX_BYTES {
            return Err(ForgeError::InvalidIndex(format!(
                "index exceeds {MAX_INDEX_BYTES} byte cap"
            )));
        }
        Self::parse(&std::fs::read(path)?)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_inner(bytes).map(|(index, _)| index)
    }

    fn parse_inner(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < HEADER_LEN + CHECKSUM_LEN || &bytes[..4] != b"DIRC" {
            return Err(ForgeError::InvalidIndex("missing DIRC header".into()));
        }
        let content_len = bytes.len() - CHECKSUM_LEN;
        let expected = &bytes[content_len..];
        let actual = Sha1::digest(&bytes[..content_len]);
        if actual.as_slice() != expected {
            return Err(ForgeError::InvalidIndex("checksum mismatch".into()));
        }

        let version = read_u32(bytes, 4)?;
        if !matches!(version, 2 | 3) {
            return Err(ForgeError::InvalidIndex(format!(
                "unsupported index version {version}; supported: 2, 3"
            )));
        }
        let count = read_u32(bytes, 8)?;
        if count > MAX_INDEX_ENTRIES {
            return Err(ForgeError::InvalidIndex(format!(
                "entry count {count} exceeds {MAX_INDEX_ENTRIES}"
            )));
        }

        let mut entries = Vec::with_capacity(count as usize);
        let mut cursor = HEADER_LEN;
        for _ in 0..count {
            let start = cursor;
            let fixed_end = cursor
                .checked_add(ENTRY_FIXED_LEN)
                .filter(|end| *end <= content_len)
                .ok_or_else(|| ForgeError::InvalidIndex("truncated entry".into()))?;
            let mode = read_u32(bytes, cursor + 24)?;
            let file_size = read_u32(bytes, cursor + 36)?;
            let mut oid_bytes = [0u8; 20];
            oid_bytes.copy_from_slice(&bytes[cursor + 40..cursor + 60]);
            let oid = ObjectId::from_bytes(oid_bytes);
            let flags = read_u16(bytes, cursor + 60)?;
            cursor = fixed_end;
            if version >= 3 && flags & 0x4000 != 0 {
                cursor = cursor
                    .checked_add(2)
                    .filter(|end| *end <= content_len)
                    .ok_or_else(|| ForgeError::InvalidIndex("truncated extended flags".into()))?;
            }
            let nul = bytes[cursor..content_len]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| cursor + offset)
                .ok_or_else(|| ForgeError::InvalidIndex("unterminated entry path".into()))?;
            let path = bytes[cursor..nul].to_vec();
            validate_index_path(&path)?;
            cursor = nul + 1;
            let consumed = cursor - start;
            let padded = consumed
                .checked_add(7)
                .map(|value| value & !7)
                .ok_or_else(|| ForgeError::InvalidIndex("entry size overflow".into()))?;
            cursor = start
                .checked_add(padded)
                .filter(|end| *end <= content_len)
                .ok_or_else(|| ForgeError::InvalidIndex("truncated entry padding".into()))?;
            entries.push(IndexEntry {
                mode,
                file_size,
                oid,
                flags,
                path,
            });
        }

        Ok((Self { version, entries }, cursor))
    }

    /// Read an index for mutation. Extensions are rejected for now because
    /// silently dropping split-index/conflict metadata would corrupt semantics.
    pub fn read_updateable(git_dir: &Path) -> Result<Self> {
        let path = git_dir.join("index");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: 2,
                    entries: Vec::new(),
                })
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(ForgeError::InvalidIndex(format!(
                "index exceeds {MAX_INDEX_BYTES} byte cap"
            )));
        }
        let (index, entries_end) = Self::parse_inner(&bytes)?;
        let content_len = bytes.len() - CHECKSUM_LEN;
        validate_droppable_extensions(&bytes, entries_end, content_len)?;
        Ok(index)
    }

    /// Replace all stages of `entry.path` with one stage-0 entry.
    pub fn upsert(&mut self, entry: IndexEntry) -> Result<()> {
        validate_index_path(&entry.path)?;
        self.entries.retain(|existing| existing.path != entry.path);
        self.entries.push(entry);
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(())
    }

    /// Remove every stage for one path. Returns whether anything changed.
    pub fn remove_path(&mut self, path: &[u8]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.path != path);
        before != self.entries.len()
    }

    /// Serialize a canonical Git index v2 with checksum.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        if self.entries.len() > MAX_INDEX_ENTRIES as usize {
            return Err(ForgeError::InvalidIndex("too many index entries".into()));
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"DIRC");
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for entry in &self.entries {
            validate_index_path(&entry.path)?;
            if entry.flags & 0x4000 != 0 {
                return Err(ForgeError::InvalidIndex(
                    "extended index flags are not mutation-safe yet".into(),
                ));
            }
            let start = out.len();
            // ctime, mtime, dev, ino: zero means Git will refresh stat data.
            for _ in 0..6 {
                out.extend_from_slice(&0u32.to_be_bytes());
            }
            out.extend_from_slice(&entry.mode.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes()); // uid
            out.extend_from_slice(&0u32.to_be_bytes()); // gid
            out.extend_from_slice(&entry.file_size.to_be_bytes());
            out.extend_from_slice(entry.oid.as_bytes());
            let path_len = entry.path.len().min(0x0fff) as u16;
            let flags = (entry.flags & 0x3000) | path_len;
            out.extend_from_slice(&flags.to_be_bytes());
            out.extend_from_slice(&entry.path);
            out.push(0);
            while (out.len() - start) % 8 != 0 {
                out.push(0);
            }
        }
        let checksum = Sha1::digest(&out);
        out.extend_from_slice(&checksum);
        Ok(out)
    }

    /// Mutate `.git/index` under the standard create-new `index.lock`, then
    /// fsync + rename it atomically. A competing or crashed writer is never
    /// overwritten.
    pub fn mutate<T>(git_dir: &Path, edit: impl FnOnce(&mut Index) -> Result<T>) -> Result<T> {
        let lock_path = git_dir.join("index.lock");
        let mut lock = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ForgeError::IndexLocked(lock_path.display().to_string()))
            }
            Err(error) => return Err(error.into()),
        };
        let mut cleanup = IndexLockCleanup::new(lock_path.clone());
        let mut index = Self::read_updateable(git_dir)?;
        let result = edit(&mut index)?;
        let encoded = index.serialize()?;
        lock.write_all(&encoded)?;
        lock.sync_all()?;
        std::fs::rename(&lock_path, git_dir.join("index"))?;
        cleanup.published = true;
        sync_directory(git_dir)?;
        Ok(result)
    }

    /// Hold the standard index lock while performing a read-only operation.
    /// The index is not rewritten; this is used by worktree restore so a
    /// concurrent staging writer cannot change the source halfway through.
    pub fn with_locked<T>(git_dir: &Path, read: impl FnOnce(&Index) -> Result<T>) -> Result<T> {
        let lock_path = git_dir.join("index.lock");
        let lock = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ForgeError::IndexLocked(lock_path.display().to_string()))
            }
            Err(error) => return Err(error.into()),
        };
        let cleanup = IndexLockCleanup::new(lock_path);
        let index = Self::read_updateable(git_dir)?;
        let result = read(&index);
        drop(lock);
        drop(cleanup);
        result
    }
}

struct IndexLockCleanup {
    path: PathBuf,
    published: bool,
}

impl IndexLockCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for IndexLockCleanup {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory = std::fs::File::open(path)?;
    directory.sync_all()?;
    Ok(())
}

pub fn validate_index_path(path: &[u8]) -> Result<()> {
    if path.is_empty()
        || path[0] == b'/'
        || path.contains(&0)
        || path.split(|byte| *byte == b'/').any(|part| {
            part.is_empty() || part == b"." || part == b".." || part.eq_ignore_ascii_case(b".git")
        })
    {
        return Err(ForgeError::UnsafePath(
            String::from_utf8_lossy(path).into_owned(),
        ));
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| ForgeError::InvalidIndex("truncated integer".into()))?
        .try_into()
        .expect("checked four-byte slice");
    Ok(u32::from_be_bytes(raw))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let raw: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| ForgeError::InvalidIndex("truncated flags".into()))?
        .try_into()
        .expect("checked two-byte slice");
    Ok(u16::from_be_bytes(raw))
}

/// Uppercase-signature extensions are optional caches and may be discarded by
/// a writer. Lowercase signatures are mandatory (notably split-index `link`)
/// and must fail closed until their semantics are implemented.
fn validate_droppable_extensions(bytes: &[u8], mut cursor: usize, end: usize) -> Result<()> {
    while cursor < end {
        let header = bytes
            .get(cursor..cursor + 8)
            .ok_or_else(|| ForgeError::InvalidIndex("truncated index extension".into()))?;
        let signature = &header[..4];
        if signature[0].is_ascii_lowercase() {
            return Err(ForgeError::InvalidIndex(format!(
                "mandatory index extension {:?} is not mutation-safe yet",
                String::from_utf8_lossy(signature)
            )));
        }
        let length = u32::from_be_bytes(header[4..8].try_into().expect("four-byte length"));
        cursor = cursor
            .checked_add(8)
            .and_then(|value| value.checked_add(length as usize))
            .filter(|value| *value <= end)
            .ok_or_else(|| ForgeError::InvalidIndex("truncated index extension data".into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_checksum_and_unsafe_paths() {
        assert!(Index::parse(b"DIRC").is_err());
        assert!(validate_index_path(b"../escape").is_err());
        assert!(validate_index_path(b"src/.git/config").is_err());
        assert!(validate_index_path(b"src/lib.rs").is_ok());
    }
}

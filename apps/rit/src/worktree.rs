//! Safe, read-only tracked-worktree status over a verified Git index.

use std::path::{Component, Path, PathBuf};

use crate::index::{validate_index_path, Index};
use crate::object::{hash_content, ObjectType, MAX_OBJECT_SIZE};
use crate::{ForgeError, ObjectId, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeState {
    Unmodified,
    Modified,
    Deleted,
    TypeChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: Vec<u8>,
    pub index_oid: ObjectId,
    pub state: WorktreeState,
}

pub fn tracked_status(worktree: &Path, index: &Index) -> Result<Vec<StatusEntry>> {
    let mut status = Vec::new();
    for entry in index.entries.iter().filter(|entry| entry.stage() == 0) {
        validate_index_path(&entry.path)?;
        let relative = bytes_to_relative_path(&entry.path)?;
        let path = worktree.join(relative);
        reject_symlink_ancestors(worktree, &path)?;
        let state = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => WorktreeState::Deleted,
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                WorktreeState::TypeChanged
            }
            Ok(metadata) if metadata.len() > MAX_OBJECT_SIZE => {
                return Err(ForgeError::ResourceLimit(format!(
                    "tracked file {} exceeds {MAX_OBJECT_SIZE} byte cap",
                    path.display()
                )))
            }
            Ok(_) => {
                let data = std::fs::read(&path)?;
                if hash_content(ObjectType::Blob, &data).0 == entry.oid {
                    WorktreeState::Unmodified
                } else {
                    WorktreeState::Modified
                }
            }
        };
        status.push(StatusEntry {
            path: entry.path.clone(),
            index_oid: entry.oid,
            state,
        });
    }
    Ok(status)
}

fn bytes_to_relative_path(path: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(path)
        .map_err(|_| ForgeError::UnsafePath("non-UTF-8 index path unsupported".into()))?;
    let candidate = Path::new(text);
    if candidate
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ForgeError::UnsafePath(text.into()));
    }
    Ok(candidate.to_path_buf())
}

fn reject_symlink_ancestors(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| ForgeError::UnsafePath(target.display().to_string()))?;
    let mut current = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        if index + 1 == component_count {
            break;
        }
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ForgeError::UnsafePath(current.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::IndexEntry;
    use std::os::unix::fs::symlink;

    #[test]
    fn tracked_status_never_follows_a_symlinked_parent() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"outside").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();
        let index = Index {
            version: 2,
            entries: vec![IndexEntry {
                mode: 0o100644,
                file_size: 7,
                oid: hash_content(ObjectType::Blob, b"outside").0,
                flags: 0,
                path: b"linked/secret".to_vec(),
            }],
        };
        assert!(matches!(
            tracked_status(root.path(), &index),
            Err(ForgeError::UnsafePath(_))
        ));
    }
}

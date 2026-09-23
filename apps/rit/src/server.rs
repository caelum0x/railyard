//! Git **hosting** surface: the small, faithful SDK a forge needs to serve a
//! repository over the *dumb* HTTP protocol.
//!
//! The dumb protocol is deliberately the simplest correct path: a client
//! fetches a plain-text ref advertisement (`info/refs`), reads `HEAD` to learn
//! the default branch, then walks the object graph by fetching individual
//! loose objects (and any packs) by their on-disk path. Because `rit` already
//! stores loose objects at `objects/xx/<rest>` and resolves refs, the only
//! genuinely git-specific thing a host must *generate* is the `info/refs`
//! advertisement — which is exactly what `git update-server-info` writes. This
//! module owns that generation so the forge never re-implements git internals.
//!
//! Nothing here starts a listener or speaks HTTP; that transport belongs to the
//! forge (`rithub-server`). This is pure content generation over a
//! [`Repository`], keeping the transport swappable and testable.

use crate::error::Result;
use crate::repo::Repository;
use std::io::Write;

/// Produce the **dumb-HTTP `info/refs`** advertisement for `repo`.
///
/// The format is one `"<40-hex-oid>\t<refname>\n"` line per ref, sorted by ref
/// name (the ordering `Refs::list` already guarantees). `HEAD` is intentionally
/// omitted — the dumb client fetches `HEAD` separately as its own resource.
///
/// This is byte-for-byte what a static `git update-server-info` produces for
/// loose repositories, so a stock `git clone`/`git fetch` over dumb HTTP can
/// discover refs and begin an object walk.
pub fn dumb_info_refs(repo: &Repository) -> Result<String> {
    let mut out = String::new();
    for (name, oid) in repo.refs().list()? {
        out.push_str(&oid.to_hex());
        out.push('\t');
        out.push_str(&name);
        out.push('\n');
    }
    Ok(out)
}

/// Produce the **`objects/info/packs`** listing for `repo`.
///
/// One `"P <pack-file-name>\n"` line per `*.pack` under `objects/pack`, followed
/// by a trailing blank line, exactly as Git writes it. A repository whose
/// objects are all loose (the common `rit`-authored case) yields an empty-but-
/// valid listing, and the dumb client simply skips straight to loose objects.
pub fn objects_info_packs(repo: &Repository) -> Result<String> {
    let pack_dir = repo.git_dir().join("objects").join("pack");
    let mut packs: Vec<String> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&pack_dir) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".pack") {
                packs.push(name);
            }
        }
    }
    // Stable output keeps the advertisement deterministic across calls.
    packs.sort();
    let mut out = String::new();
    for name in packs {
        out.push_str("P ");
        out.push_str(&name);
        out.push('\n');
    }
    out.push('\n');
    Ok(out)
}

/// Write the on-disk `info/refs` and `objects/info/packs` files — the exact
/// artifacts `git update-server-info` maintains so a *plain static file server*
/// can also host the repository over dumb HTTP.
///
/// A forge that serves these resources dynamically does not strictly need this,
/// but calling it after each ref change keeps the on-disk repository a valid
/// standalone dumb-HTTP root (e.g. for backup, mirroring, or a fallback static
/// server). Writes are best-effort-atomic: temp file, fsync, rename.
pub fn update_server_info(repo: &Repository) -> Result<()> {
    let git_dir = repo.git_dir();
    let info_dir = git_dir.join("info");
    std::fs::create_dir_all(&info_dir)?;
    write_atomic(&info_dir.join("refs"), dumb_info_refs(repo)?.as_bytes())?;

    let objects_info = git_dir.join("objects").join("info");
    std::fs::create_dir_all(&objects_info)?;
    write_atomic(
        &objects_info.join("packs"),
        objects_info_packs(repo)?.as_bytes(),
    )?;
    Ok(())
}

/// Atomically replace `path` with `bytes` (temp file in the same directory,
/// fsync, rename) so a concurrent reader never observes a torn advertisement.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let tmp = parent.join(format!(
        ".rit-serverinfo-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;
    use crate::Signature;

    fn sig() -> Signature {
        Signature::parse("Tester <t@example.com> 1700000000 +0000", "commit").unwrap()
    }

    #[test]
    fn dumb_info_refs_lists_seeded_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob = repo.hash_object(ObjectType::Blob, b"hi\n", true).unwrap();
        let mut tree = crate::Tree {
            entries: vec![crate::tree::TreeEntry {
                mode: "100644".into(),
                name: b"a.txt".to_vec(),
                oid: blob,
            }],
        };
        tree.canonicalize();
        let tree_oid = repo
            .hash_object(ObjectType::Tree, &tree.serialize(), true)
            .unwrap();
        let commit = repo
            .commit_tree(
                tree_oid,
                Vec::new(),
                sig(),
                sig(),
                b"seed\n",
                Some("refs/heads/main"),
            )
            .unwrap();

        let advertised = dumb_info_refs(&repo).unwrap();
        assert_eq!(
            advertised,
            format!("{}\trefs/heads/main\n", commit.to_hex())
        );

        // update_server_info writes the same content to disk.
        update_server_info(&repo).unwrap();
        let on_disk = std::fs::read_to_string(repo.git_dir().join("info").join("refs")).unwrap();
        assert_eq!(on_disk, advertised);
        assert!(repo
            .git_dir()
            .join("objects")
            .join("info")
            .join("packs")
            .exists());
    }
}

//! Real-git interoperability tests — the whole point of this slice.
//!
//! These shell out to the installed `git` binary and assert that:
//!   1. object ids we compute are byte-identical to `git hash-object`;
//!   2. a repository authored by real `git` is read back correctly by us;
//!   3. objects we write are readable by real `git` (`cat-file`), and
//!   4. our serialization reproduces git's exact bytes (round-trip via oid).
//!
//! Requires `git` on PATH (the task environment has it).

use std::path::Path;
use std::process::Command;

use rit::object::{hash_content, Object};
use rit::{ObjectId, ObjectType, Repository};
use tempfile::TempDir;

/// Run `git` with args in `cwd`, returning trimmed stdout; panics on failure.
fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git must be installed and on PATH");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Run `git` returning raw (untrimmed) stdout bytes.
fn git_bytes(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git must be installed and on PATH");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Create an initialized repo with deterministic identity + a first commit.
fn init_repo_with_commit() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.name", "Forge Tester"]);
    git(p, &["config", "user.email", "forge@example.com"]);
    git(p, &["config", "commit.gpgsign", "false"]);

    std::fs::write(p.join("hello.txt"), b"Hello, Forge!\n").unwrap();
    std::fs::create_dir(p.join("sub")).unwrap();
    std::fs::write(p.join("sub").join("nested.txt"), b"nested content\n").unwrap();
    git(p, &["add", "."]);
    // Fixed dates so commits are reproducible.
    let env_commit = [
        "-c",
        "user.name=Forge Tester",
        "-c",
        "user.email=forge@example.com",
    ];
    let mut args = vec!["commit", "-q", "-m", "initial commit"];
    // prepend the -c overrides
    let mut full: Vec<&str> = env_commit.to_vec();
    full.append(&mut args);
    git(p, &full);
    dir
}

/// The deterministic `-c` identity overrides used for reproducible commits.
const ID_OVERRIDES: [&str; 4] = [
    "-c",
    "user.name=Forge Tester",
    "-c",
    "user.email=forge@example.com",
];

/// Commit the current index with the fixed identity and the given message.
fn commit_all(p: &Path, message: &str) {
    git(p, &["add", "."]);
    let mut args: Vec<&str> = ID_OVERRIDES.to_vec();
    args.extend_from_slice(&["commit", "-q", "-m", message]);
    git(p, &args);
}

/// Force **every reachable object into a single packfile** and drop the loose
/// copies — the real-git way to move objects out of loose storage and into a
/// pack (`git repack -a -d` builds the pack; `git prune-packed` removes the now
/// redundant loose objects). After this, any object we can still read *must*
/// have come from the packfile.
fn pack_everything(p: &Path) {
    git(p, &["repack", "-a", "-d"]);
    git(p, &["prune-packed"]);
}

/// Assert a packfile physically exists under `.git/objects/pack`.
fn assert_has_packfile(p: &Path) {
    let pack_dir = p.join(".git").join("objects").join("pack");
    let has_pack = std::fs::read_dir(&pack_dir)
        .expect("pack directory should exist after repack")
        .flatten()
        .any(|e| e.path().extension().map(|x| x == "pack").unwrap_or(false));
    assert!(has_pack, "expected a .pack file under {pack_dir:?}");
}

#[test]
fn blob_oid_matches_git_hash_object() {
    let dir = tempfile::tempdir().unwrap();
    let contents: &[&[u8]] = &[
        b"hello world\n",
        b"",
        b"no trailing newline",
        b"\x00\x01\x02binary",
    ];

    for (i, content) in contents.iter().enumerate() {
        let file = dir.path().join(format!("f{i}.bin"));
        std::fs::write(&file, content).unwrap();

        let git_oid = git(dir.path(), &["hash-object", file.to_str().unwrap()]);
        let ours = hash_content(ObjectType::Blob, content).0.to_hex();

        assert_eq!(
            ours, git_oid,
            "blob oid mismatch for content #{i} ({content:?})"
        );
    }
}

#[test]
fn reads_repo_authored_by_real_git() {
    let dir = init_repo_with_commit();
    let p = dir.path();

    let repo = Repository::discover(p).unwrap();

    // HEAD oid matches `git rev-parse HEAD`.
    let head = repo.head().unwrap();
    assert_eq!(head.to_hex(), git(p, &["rev-parse", "HEAD"]));

    // The HEAD commit's tree matches `git rev-parse HEAD^{tree}`.
    let commit = repo.head_commit().unwrap();
    assert_eq!(commit.tree.to_hex(), git(p, &["rev-parse", "HEAD^{tree}"]));

    // Parsed metadata is correct.
    assert_eq!(commit.author.name, "Forge Tester");
    assert_eq!(commit.author.email, "forge@example.com");
    assert_eq!(commit.summary(), "initial commit");
    assert!(commit.parents.is_empty(), "first commit has no parents");

    // The tree lists hello.txt with the same blob oid git reports.
    let tree = repo.tree(&commit.tree).unwrap();
    let entry = tree.entry(b"hello.txt").expect("hello.txt in tree");
    assert_eq!(entry.oid.to_hex(), git(p, &["rev-parse", "HEAD:hello.txt"]));
    assert_eq!(entry.display_mode(), "100644");

    // And the blob content round-trips.
    let blob = repo.blob(&entry.oid).unwrap();
    assert_eq!(blob.data, b"Hello, Forge!\n");

    // Sub-tree is reachable and its nested blob matches git.
    let sub = tree.entry(b"sub").expect("sub dir in tree");
    assert_eq!(sub.kind(), "tree");
    let subtree = repo.tree(&sub.oid).unwrap();
    let nested = subtree.entry(b"nested.txt").expect("nested.txt");
    assert_eq!(
        nested.oid.to_hex(),
        git(p, &["rev-parse", "HEAD:sub/nested.txt"])
    );
}

#[test]
fn objects_we_write_are_readable_by_real_git() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    let repo = Repository::discover(p).unwrap();

    // Write a blob with OUR code.
    let content = b"round trip through our writer\n";
    let oid = repo.hash_object(ObjectType::Blob, content, true).unwrap();

    // Real git can read its type and content.
    assert_eq!(git(p, &["cat-file", "-t", &oid.to_hex()]), "blob");
    assert_eq!(git_bytes(p, &["cat-file", "-p", &oid.to_hex()]), content);

    // And git's own oid for the same content agrees with ours.
    let git_oid = {
        let f = p.join("rt.txt");
        std::fs::write(&f, content).unwrap();
        git(p, &["hash-object", f.to_str().unwrap()])
    };
    assert_eq!(oid.to_hex(), git_oid);

    // fsck is clean: nothing we wrote corrupts the repo.
    let fsck = git(p, &["fsck", "--strict"]);
    assert!(
        !fsck.to_lowercase().contains("error"),
        "git fsck reported problems: {fsck}"
    );
}

#[test]
fn our_serialization_reproduces_git_bytes_exactly() {
    // If our parse+serialize is byte-exact, re-hashing git's objects with our
    // code must reproduce git's oids.
    let dir = init_repo_with_commit();
    let p = dir.path();
    let repo = Repository::discover(p).unwrap();

    let head = repo.head().unwrap();

    // Commit: parse then re-serialize then re-hash -> same oid.
    match repo.read_object(&head).unwrap() {
        Object::Commit(c) => {
            let (id, _) = hash_content(ObjectType::Commit, &c.serialize());
            assert_eq!(id, head, "commit re-serialization is not byte-exact");
        }
        other => panic!("HEAD should be a commit, got {other:?}"),
    }

    // Tree: same property.
    let tree_oid = repo.head_commit().unwrap().tree;
    let tree = repo.tree(&tree_oid).unwrap();
    let (id, _) = hash_content(ObjectType::Tree, &tree.serialize());
    assert_eq!(id, tree_oid, "tree re-serialization is not byte-exact");
}

#[test]
fn reads_annotated_tag_from_real_git() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    git(
        p,
        &[
            "-c",
            "user.name=Forge Tester",
            "-c",
            "user.email=forge@example.com",
            "tag",
            "-a",
            "v1.0.0",
            "-m",
            "first release",
        ],
    );

    let repo = Repository::discover(p).unwrap();
    let tag_oid = repo.resolve("v1.0.0").unwrap();
    match repo.read_object(&tag_oid).unwrap() {
        Object::Tag(t) => {
            assert_eq!(t.tag, "v1.0.0");
            assert_eq!(t.object_type, ObjectType::Commit);
            assert_eq!(t.object, repo.head().unwrap());
            assert!(t.message_str().contains("first release"));
            // byte-exact: re-hash reproduces git's tag oid
            let (id, _) = hash_content(ObjectType::Tag, &t.serialize());
            assert_eq!(id, tag_oid, "tag re-serialization is not byte-exact");
        }
        other => panic!("expected annotated tag, got {other:?}"),
    }

    // Peeling the tag reaches the commit.
    let commit = repo.peel_to_commit(tag_oid).unwrap();
    assert_eq!(commit.summary(), "initial commit");
}

#[test]
fn log_walks_multi_commit_history() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    let overrides = [
        "-c",
        "user.name=Forge Tester",
        "-c",
        "user.email=forge@example.com",
    ];

    // second commit
    std::fs::write(p.join("second.txt"), b"second\n").unwrap();
    git(p, &["add", "."]);
    let mut a: Vec<&str> = overrides.to_vec();
    a.extend_from_slice(&["commit", "-q", "-m", "second commit"]);
    git(p, &a);

    // third commit
    std::fs::write(p.join("third.txt"), b"third\n").unwrap();
    git(p, &["add", "."]);
    let mut b: Vec<&str> = overrides.to_vec();
    b.extend_from_slice(&["commit", "-q", "-m", "third commit"]);
    git(p, &b);

    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();
    let history = repo.log(head).unwrap();

    assert_eq!(history.len(), 3, "expected 3 commits");
    // Compare our ordered oids against `git rev-list HEAD`.
    let git_list: Vec<String> = git(p, &["rev-list", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    let ours: Vec<String> = history.iter().map(|(oid, _)| oid.to_hex()).collect();
    assert_eq!(
        ours, git_list,
        "log order/content differs from git rev-list"
    );
    assert_eq!(history[0].1.summary(), "third commit");
}

#[test]
fn corrupt_object_is_rejected_not_panicked() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();

    // Overwrite the loose object with garbage and confirm we error, not panic.
    // Git stores loose objects read-only (0444), so make it writable first.
    let path = repo.odb().loose_path(&head);
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::fs::write(&path, b"totally not a git object").unwrap();
        let result = repo.read_object(&head);
        assert!(result.is_err(), "corrupt object must be rejected");
    }
}

// ---------------------------------------------------------------------------
// Packfile interop — the objects are read from a PACKED repository (via the
// gitoxide-backed pack path), and every result is cross-checked against real
// `git`. Each test first proves the object is no longer loose, so a pass can
// only mean the packfile reader worked.
// ---------------------------------------------------------------------------

#[test]
fn reads_commit_tree_blob_from_packfile() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    pack_everything(p);
    assert_has_packfile(p);

    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();

    // Prove the object is NOT loose: it can only be served from the packfile.
    assert!(
        !repo.odb().loose_path(&head).exists(),
        "HEAD should be packed, but a loose object still exists"
    );
    assert_eq!(head.to_hex(), git(p, &["rev-parse", "HEAD"]));

    // Commit decoded from the pack matches git.
    let commit = repo.commit(&head).unwrap();
    assert_eq!(commit.tree.to_hex(), git(p, &["rev-parse", "HEAD^{tree}"]));
    assert_eq!(commit.author.name, "Forge Tester");
    assert_eq!(commit.summary(), "initial commit");

    // Tree decoded from the pack matches git, and its entry oids match.
    let tree = repo.tree(&commit.tree).unwrap();
    assert!(
        !repo.odb().loose_path(&commit.tree).exists(),
        "tree should be packed"
    );
    let entry = tree.entry(b"hello.txt").expect("hello.txt in tree");
    assert_eq!(entry.oid.to_hex(), git(p, &["rev-parse", "HEAD:hello.txt"]));

    // Blob decoded from the pack matches git's content byte-for-byte.
    let blob = repo.blob(&entry.oid).unwrap();
    assert!(
        !repo.odb().loose_path(&entry.oid).exists(),
        "blob should be packed"
    );
    assert_eq!(blob.data, b"Hello, Forge!\n");
    assert_eq!(
        git_bytes(p, &["cat-file", "-p", &entry.oid.to_hex()]),
        blob.data
    );

    // A packed sub-tree and its nested blob also match git.
    let sub = tree.entry(b"sub").expect("sub dir in tree");
    assert_eq!(sub.kind(), "tree");
    let subtree = repo.tree(&sub.oid).unwrap();
    let nested = subtree.entry(b"nested.txt").expect("nested.txt");
    assert_eq!(
        nested.oid.to_hex(),
        git(p, &["rev-parse", "HEAD:sub/nested.txt"])
    );
    assert_eq!(repo.blob(&nested.oid).unwrap().data, b"nested content\n");
}

#[test]
fn packed_objects_reserialize_to_same_oids() {
    // Parsing a packed object then re-serializing must reproduce git's exact
    // bytes — i.e. our oid recomputation matches, proving faithful decoding.
    let dir = init_repo_with_commit();
    let p = dir.path();
    pack_everything(p);

    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();
    assert!(
        !repo.odb().loose_path(&head).exists(),
        "HEAD should be packed"
    );

    match repo.read_object(&head).unwrap() {
        Object::Commit(c) => {
            let (id, _) = hash_content(ObjectType::Commit, &c.serialize());
            assert_eq!(id, head, "packed commit re-serialization is not byte-exact");
        }
        other => panic!("HEAD should be a commit, got {other:?}"),
    }

    let tree_oid = repo.head_commit().unwrap().tree;
    let tree = repo.tree(&tree_oid).unwrap();
    let (id, _) = hash_content(ObjectType::Tree, &tree.serialize());
    assert_eq!(
        id, tree_oid,
        "packed tree re-serialization is not byte-exact"
    );
}

#[test]
fn reads_deltified_blobs_across_history_from_packfile() {
    // Build an evolving file across several commits so `git repack` stores the
    // blob versions as a DELTA CHAIN, then read every historical version back
    // from the pack and match git. This is the delta-resolution path that is
    // the whole point of adopting gitoxide.
    let dir = init_repo_with_commit();
    let p = dir.path();

    let mut expected: Vec<(String, String)> = Vec::new();
    let mut content = String::new();
    for i in 0..8 {
        content.push_str(&format!(
            "line {i} ------------------------------------------------------------\n"
        ));
        std::fs::write(p.join("data.txt"), &content).unwrap();
        commit_all(p, &format!("rev {i}"));
        let blob_oid = git(p, &["rev-parse", "HEAD:data.txt"]);
        expected.push((blob_oid, content.clone()));
    }

    // Aggressive repack to encourage deep delta chains, then drop loose copies.
    git(
        p,
        &["repack", "-a", "-d", "-f", "--depth=50", "--window=50"],
    );
    git(p, &["prune-packed"]);
    assert_has_packfile(p);

    let repo = Repository::discover(p).unwrap();
    for (blob_hex, want) in &expected {
        let oid = ObjectId::from_hex(blob_hex).unwrap();
        assert!(
            !repo.odb().loose_path(&oid).exists(),
            "blob {blob_hex} should be packed (delta chain)"
        );
        let blob = repo.blob(&oid).unwrap();
        assert_eq!(
            blob.data,
            want.as_bytes(),
            "packed/deltified blob {blob_hex} content mismatch"
        );
        assert_eq!(git_bytes(p, &["cat-file", "-p", blob_hex]), want.as_bytes());
    }
}

#[test]
fn contains_verify_and_log_work_on_packed_repo() {
    let dir = init_repo_with_commit();
    let p = dir.path();

    // Two more commits so the packed history is non-trivial.
    std::fs::write(p.join("second.txt"), b"second\n").unwrap();
    commit_all(p, "second commit");
    std::fs::write(p.join("third.txt"), b"third\n").unwrap();
    commit_all(p, "third commit");

    pack_everything(p);
    assert_has_packfile(p);

    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();
    assert!(
        !repo.odb().loose_path(&head).exists(),
        "HEAD should be packed"
    );

    // contains() and verify() both see through to the packfile.
    assert!(
        repo.odb().contains(&head),
        "packed object must be contained"
    );
    assert!(repo.verify(&head).unwrap(), "packed object must verify");

    // Missing objects are still reported as absent, not errors.
    let absent = ObjectId::from_hex("0123456789012345678901234567890123456789").unwrap();
    assert!(!repo.odb().contains(&absent));

    // `log` walks the fully-packed history and matches `git rev-list HEAD`.
    let history = repo.log(head).unwrap();
    let git_list: Vec<String> = git(p, &["rev-list", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    let ours: Vec<String> = history.iter().map(|(oid, _)| oid.to_hex()).collect();
    assert_eq!(ours, git_list, "packed log order differs from git rev-list");
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].1.summary(), "third commit");
}

#[test]
fn reads_annotated_tag_from_packfile() {
    let dir = init_repo_with_commit();
    let p = dir.path();
    git(
        p,
        &[
            "-c",
            "user.name=Forge Tester",
            "-c",
            "user.email=forge@example.com",
            "tag",
            "-a",
            "v1.0.0",
            "-m",
            "first release",
        ],
    );
    pack_everything(p);
    assert_has_packfile(p);

    let repo = Repository::discover(p).unwrap();
    let tag_oid = repo.resolve("v1.0.0").unwrap();
    assert!(
        !repo.odb().loose_path(&tag_oid).exists(),
        "annotated tag object should be packed"
    );
    match repo.read_object(&tag_oid).unwrap() {
        Object::Tag(t) => {
            assert_eq!(t.tag, "v1.0.0");
            assert_eq!(t.object_type, ObjectType::Commit);
            assert_eq!(t.object, repo.head().unwrap());
            assert!(t.message_str().contains("first release"));
            let (id, _) = hash_content(ObjectType::Tag, &t.serialize());
            assert_eq!(id, tag_oid, "packed tag re-serialization is not byte-exact");
        }
        other => panic!("expected annotated tag, got {other:?}"),
    }
    // Peeling the packed tag reaches the packed commit.
    assert_eq!(
        repo.peel_to_commit(tag_oid).unwrap().summary(),
        "initial commit"
    );
}

#[test]
fn corrupt_packfile_is_rejected_not_panicked() {
    // A truncated/garbled .pack must fail closed (typed error), never panic.
    let dir = init_repo_with_commit();
    let p = dir.path();
    pack_everything(p);

    let repo = Repository::discover(p).unwrap();
    let head = repo.head().unwrap();
    assert!(
        !repo.odb().loose_path(&head).exists(),
        "HEAD should be packed"
    );

    // Corrupt every .pack by overwriting its body with garbage (keep the file
    // so the index still points into it).
    let pack_dir = p.join(".git").join("objects").join("pack");
    for entry in std::fs::read_dir(&pack_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "pack").unwrap_or(false) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
            }
            std::fs::write(&path, b"PACK\xff\xff\xff\xffnot a real packfile body").unwrap();
        }
    }

    // A fresh repo handle so no cached mmap masks the corruption.
    let repo2 = Repository::discover(p).unwrap();
    let result = repo2.read_object(&head);
    assert!(
        result.is_err(),
        "reading from a corrupt packfile must return an error, not succeed or panic"
    );
}

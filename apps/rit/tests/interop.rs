//! Real-git interoperability tests — the whole point of this slice.
//!
//! These shell out to the installed `git` binary and assert that:
//!   1. object ids we compute are byte-identical to `git hash-object`;
//!   2. a repository authored by real `git` is read back correctly by us;
//!   3. objects we write are readable by real `git` (`cat-file`), and
//!   4. our serialization reproduces git's exact bytes (round-trip via oid).
//!
//! Requires `git` on PATH (the task environment has it).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use rit::object::{hash_content, Object};
use rit::{
    decode_packet_lines, encode_packet_line, execute_receive_pack, ForgeError, Index, ObjectId,
    ObjectType, PacketLine, PacketLineLimits, ReceiveLimits, ReceiveObject, ReceivePackPolicy,
    ReceiveTransaction, RefUpdate, Repository, Signature, WorktreeState,
};
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

/// Run Git and require a non-zero result, returning stderr for assertions.
fn git_fails(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git must be installed and on PATH");
    assert!(
        !output.status.success(),
        "git {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run Git with binary stdin and return binary stdout.
fn git_bytes_with_stdin(cwd: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git must be installed and on PATH");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin)
        .expect("write git stdin");
    let output = child.wait_with_output().expect("wait for git");
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
fn quarantined_receive_publishes_a_real_git_graph_that_git_fsck_accepts() {
    let source = init_repo_with_commit();
    let source_repo = Repository::discover(source.path()).unwrap();
    let reachable = git(source.path(), &["rev-list", "--objects", "--all"]);
    let ids = reachable
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|hex| ObjectId::from_hex(hex).unwrap())
        .collect::<Vec<_>>();
    assert!(
        ids.len() >= 5,
        "fixture should include commit/tree/blob graph"
    );

    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "--bare", "-q"]);
    let target_repo = Repository::open_git_dir(target.path()).unwrap();
    let mut receive = ReceiveTransaction::begin(&target_repo, ReceiveLimits::default()).unwrap();
    for oid in &ids {
        let (object_type, payload) = source_repo.odb().read_raw(oid).unwrap();
        receive
            .stage(ReceiveObject {
                declared_oid: *oid,
                object_type,
                payload,
            })
            .unwrap();
    }
    let tip = source_repo.refs().resolve("refs/heads/main").unwrap();
    let outcome = receive
        .finish(RefUpdate {
            name: "refs/heads/main".into(),
            expected_old: None,
            new: tip,
            allow_non_fast_forward: false,
        })
        .unwrap();

    assert_eq!(outcome.published_objects, ids.len());
    assert_eq!(
        git(target.path(), &["rev-parse", "refs/heads/main"]),
        tip.to_hex()
    );
    git(target.path(), &["fsck", "--strict"]);
    assert!(std::fs::read_dir(target.path().join("rit-quarantine"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn raw_git_pack_is_verified_decoded_and_published_through_quarantine() {
    let source = init_repo_with_commit();
    let root = source.path();

    // Similar successive payloads give stock Git useful delta candidates. Rit
    // does not implement those delta instructions itself: gitoxide resolves
    // them before the normal decoded-object quarantine boundary.
    for revision in 0..4 {
        let mut payload = vec![b'a'; 32 * 1024];
        payload[revision * 101] = b'b' + revision as u8;
        std::fs::write(root.join("large.txt"), payload).unwrap();
        commit_all(root, &format!("large revision {revision}"));
    }
    let pack = git_bytes_with_stdin(
        root,
        &[
            "pack-objects",
            "--stdout",
            "--revs",
            "--delta-base-offset",
            "--window=10",
            "--depth=10",
        ],
        b"refs/heads/main\n",
    );
    assert_eq!(&pack[..4], b"PACK");

    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "--bare", "-q"]);
    let target_repo = Repository::open_git_dir(target.path()).unwrap();
    let mut receive = ReceiveTransaction::begin(&target_repo, ReceiveLimits::default()).unwrap();
    let imported = receive.stage_pack(&pack).unwrap();
    let tip = ObjectId::from_hex(&git(root, &["rev-parse", "refs/heads/main"])).unwrap();
    let outcome = receive
        .finish(RefUpdate {
            name: "refs/heads/main".into(),
            expected_old: None,
            new: tip,
            allow_non_fast_forward: false,
        })
        .unwrap();

    assert!(imported >= 10, "fixture should produce a non-trivial pack");
    assert_eq!(outcome.published_objects, imported);
    assert_eq!(
        git(target.path(), &["rev-parse", "refs/heads/main"]),
        tip.to_hex()
    );
    git(target.path(), &["fsck", "--strict"]);
    assert!(std::fs::read_dir(target.path().join("rit-quarantine"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn complete_receive_pack_body_moves_one_ref_and_returns_report_status() {
    let source = init_repo_with_commit();
    let pack = git_bytes_with_stdin(
        source.path(),
        &["pack-objects", "--stdout", "--revs", "--delta-base-offset"],
        b"refs/heads/main\n",
    );
    let tip = git(source.path(), &["rev-parse", "refs/heads/main"]);
    let zero = "0000000000000000000000000000000000000000";
    let command = format!(
        "{zero} {tip} refs/heads/main\0report-status-v2 object-format=sha1 agent=git/test\n"
    );
    let mut request = encode_packet_line(&PacketLine::Data(command.into_bytes())).unwrap();
    request.extend_from_slice(b"0000");
    request.extend_from_slice(&pack);

    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "--bare", "-q"]);
    let target_repo = Repository::open_git_dir(target.path()).unwrap();
    let execution = execute_receive_pack(
        &target_repo,
        &request,
        &["report-status-v2", "object-format=sha1", "agent=rit/0.1"],
        PacketLineLimits::default(),
        ReceiveLimits::default(),
        ReceivePackPolicy::default(),
    )
    .unwrap();

    assert!(execution.rejection.is_none());
    assert_eq!(execution.outcome.unwrap().new.to_hex(), tip);
    let response = decode_packet_lines(&execution.response, PacketLineLimits::default()).unwrap();
    assert!(matches!(&response[0], PacketLine::Data(line) if line == b"unpack ok\n"));
    assert!(matches!(&response[1], PacketLine::Data(line) if line == b"ok refs/heads/main\n"));
    assert_eq!(git(target.path(), &["rev-parse", "refs/heads/main"]), tip);
    git(target.path(), &["fsck", "--strict"]);
}

#[test]
fn stock_git_pushes_create_and_update_through_rit_receive_pack_process() {
    let source = init_repo_with_commit();
    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "--bare", "-q"]);
    let receive_pack = format!("{} receive-pack", env!("CARGO_BIN_EXE_rit"));
    let target_path = target.path().to_str().unwrap();

    git(
        source.path(),
        &[
            "push",
            "--quiet",
            "--receive-pack",
            &receive_pack,
            target_path,
            "main:main",
        ],
    );
    let first = git(source.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git(target.path(), &["rev-parse", "refs/heads/main"]), first);

    std::fs::write(
        source.path().join("hello.txt"),
        b"second push through rit\n",
    )
    .unwrap();
    commit_all(source.path(), "second push");
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            "--receive-pack",
            &receive_pack,
            target_path,
            "main:main",
        ],
    );
    let second = git(source.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        git(target.path(), &["rev-parse", "refs/heads/main"]),
        second
    );

    git(source.path(), &["reset", "--hard", "HEAD~1"]);
    std::fs::write(source.path().join("divergent.txt"), b"rewrite attempt\n").unwrap();
    commit_all(source.path(), "divergent rewrite");
    let error = git_fails(
        source.path(),
        &[
            "push",
            "--force",
            "--receive-pack",
            &receive_pack,
            target_path,
            "main:main",
        ],
    );
    assert!(
        error.contains("non-fast-forward") || error.contains("remote rejected"),
        "unexpected force-push error: {error}"
    );
    assert_eq!(
        git(target.path(), &["rev-parse", "refs/heads/main"]),
        second,
        "client --force must not override server history policy"
    );
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            "--receive-pack",
            &receive_pack,
            target_path,
            ":main",
        ],
    );
    let deletion_check = git_fails(target.path(), &["show-ref", "--verify", "refs/heads/main"]);
    assert!(deletion_check.is_empty() || deletion_check.contains("refs/heads/main"));
    git(target.path(), &["fsck", "--strict"]);
}

#[test]
fn stock_git_clones_and_fetches_through_rit_upload_pack_bridge() {
    let source = init_repo_with_commit();
    let transport_root = tempfile::tempdir().unwrap();
    let bare = transport_root.path().join("origin.git");
    let checkout = transport_root.path().join("checkout");
    git(
        transport_root.path(),
        &[
            "clone",
            "--bare",
            "--quiet",
            source.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let upload_pack = format!("{} upload-pack", env!("CARGO_BIN_EXE_rit"));
    git(
        transport_root.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            "--upload-pack",
            &upload_pack,
            bare.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        git(source.path(), &["rev-parse", "HEAD"])
    );

    std::fs::write(source.path().join("fetched.txt"), b"through upload-pack\n").unwrap();
    commit_all(source.path(), "fetched through bridge");
    git(
        source.path(),
        &["push", "--quiet", bare.to_str().unwrap(), "main:main"],
    );
    git(
        &checkout,
        &["fetch", "--quiet", "--upload-pack", &upload_pack, "origin"],
    );
    assert_eq!(
        git(&checkout, &["rev-parse", "origin/main"]),
        git(source.path(), &["rev-parse", "HEAD"])
    );
    git(bare.as_path(), &["fsck", "--strict"]);
}

#[test]
fn rit_receive_pack_refuses_deleting_a_packed_ref_without_exposing_old_state() {
    let source = init_repo_with_commit();
    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "--bare", "-q"]);
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            target.path().to_str().unwrap(),
            "main:main",
        ],
    );
    let tip = git(target.path(), &["rev-parse", "refs/heads/main"]);
    git(target.path(), &["pack-refs", "--all", "--prune"]);
    assert!(!target.path().join("refs/heads/main").exists());

    let receive_pack = format!("{} receive-pack", env!("CARGO_BIN_EXE_rit"));
    let error = git_fails(
        source.path(),
        &[
            "push",
            "--receive-pack",
            &receive_pack,
            target.path().to_str().unwrap(),
            ":main",
        ],
    );
    assert!(
        error.contains("remote rejected") || error.contains("receive policy rejected"),
        "unexpected packed-ref deletion error: {error}"
    );
    assert_eq!(git(target.path(), &["rev-parse", "refs/heads/main"]), tip);
    assert!(!target.path().join("refs/heads/main.lock").exists());
}

#[test]
fn corrupt_or_truncated_raw_pack_never_publishes_objects_or_refs() {
    let source = init_repo_with_commit();
    let pack = git_bytes_with_stdin(
        source.path(),
        &["pack-objects", "--stdout", "--revs"],
        b"refs/heads/main\n",
    );
    let tip = ObjectId::from_hex(&git(source.path(), &["rev-parse", "HEAD"])).unwrap();

    for bad_pack in [
        {
            let mut corrupt = pack.clone();
            let last = corrupt.len() - 1;
            corrupt[last] ^= 0xff;
            corrupt
        },
        pack[..pack.len() - 1].to_vec(),
    ] {
        let target = tempfile::tempdir().unwrap();
        git(target.path(), &["init", "--bare", "-q"]);
        let target_repo = Repository::open_git_dir(target.path()).unwrap();
        let mut receive =
            ReceiveTransaction::begin(&target_repo, ReceiveLimits::default()).unwrap();
        assert!(matches!(
            receive.stage_pack(&bad_pack),
            Err(ForgeError::Pack(_))
        ));
        assert!(!target_repo.odb().contains(&tip));
        assert!(target_repo.refs().resolve("refs/heads/main").is_err());
    }
}

#[test]
fn thin_git_pack_resolves_verified_bases_from_target_odb() {
    let source = init_repo_with_commit();
    let root = source.path();
    let mut base_payload = vec![b'x'; 64 * 1024];
    base_payload[17] = b'a';
    std::fs::write(root.join("delta-base.txt"), &base_payload).unwrap();
    commit_all(root, "delta base");
    let old = git(root, &["rev-parse", "HEAD"]);

    let target = tempfile::tempdir().unwrap();
    git(
        target.path(),
        &["clone", "--bare", "-q", root.to_str().unwrap(), "."],
    );

    for revision in 0..3 {
        base_payload[1000 + revision * 37] = b'b' + revision as u8;
        std::fs::write(root.join("delta-base.txt"), &base_payload).unwrap();
        commit_all(root, &format!("thin revision {revision}"));
    }
    let new = git(root, &["rev-parse", "HEAD"]);
    let revisions = format!("{new}\n^{old}\n");
    let thin_pack = git_bytes_with_stdin(
        root,
        &[
            "pack-objects",
            "--stdout",
            "--revs",
            "--thin",
            "--delta-base-offset",
            "--window=10",
            "--depth=10",
        ],
        revisions.as_bytes(),
    );

    // The same transport pack must fail in a repository that does not own its
    // advertised base. This also proves the fixture is genuinely thin rather
    // than merely requested with `--thin` but self-contained by chance.
    let empty_target = tempfile::tempdir().unwrap();
    git(empty_target.path(), &["init", "--bare", "-q"]);
    let empty_repo = Repository::open_git_dir(empty_target.path()).unwrap();
    let mut missing_base =
        ReceiveTransaction::begin(&empty_repo, ReceiveLimits::default()).unwrap();
    assert!(matches!(
        missing_base.stage_pack(&thin_pack),
        Err(ForgeError::Pack(_))
    ));
    assert!(empty_repo.refs().resolve("refs/heads/main").is_err());

    let target_repo = Repository::open_git_dir(target.path()).unwrap();
    let mut receive = ReceiveTransaction::begin(&target_repo, ReceiveLimits::default()).unwrap();
    let staged = receive.stage_pack(&thin_pack).unwrap();
    assert!(staged > 0);
    receive
        .finish(RefUpdate {
            name: "refs/heads/main".into(),
            expected_old: Some(ObjectId::from_hex(&old).unwrap()),
            new: ObjectId::from_hex(&new).unwrap(),
            allow_non_fast_forward: false,
        })
        .unwrap();

    assert_eq!(git(target.path(), &["rev-parse", "refs/heads/main"]), new);
    git(target.path(), &["fsck", "--strict"]);
}

#[test]
fn reads_real_git_index_and_reports_tracked_worktree_changes() {
    let dir = init_repo_with_commit();
    let repo = Repository::discover(dir.path()).unwrap();
    let index = repo.index().expect("real Git v2 index should parse");
    let paths: Vec<_> = index.entries.iter().map(|entry| entry.path_str()).collect();
    assert_eq!(paths, vec!["hello.txt", "sub/nested.txt"]);
    assert!(repo
        .tracked_status()
        .unwrap()
        .iter()
        .all(|entry| entry.state == WorktreeState::Unmodified));

    std::fs::write(dir.path().join("hello.txt"), b"modified\n").unwrap();
    std::fs::remove_file(dir.path().join("sub/nested.txt")).unwrap();
    let changed = repo.tracked_status().unwrap();
    assert_eq!(changed[0].state, WorktreeState::Modified);
    assert_eq!(changed[1].state, WorktreeState::Deleted);

    let index_path = dir.path().join(".git/index");
    let mut corrupt = std::fs::read(&index_path).unwrap();
    corrupt[16] ^= 0xff;
    std::fs::write(index_path, corrupt).unwrap();
    assert!(matches!(repo.index(), Err(ForgeError::InvalidIndex(_))));
}

#[test]
fn rit_add_writes_an_index_real_git_can_commit() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::write(root.join("hello.txt"), b"staged by rit\n").unwrap();
    std::fs::write(root.join("new.txt"), b"new through rit\n").unwrap();

    let repo = Repository::discover(root).unwrap();
    assert_eq!(
        repo.add(&["hello.txt".into(), "new.txt".into()]).unwrap(),
        2
    );

    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "hello.txt\nnew.txt"
    );
    assert_eq!(git_bytes(root, &["show", ":hello.txt"]), b"staged by rit\n");
    assert_eq!(git_bytes(root, &["show", ":new.txt"]), b"new through rit\n");
    let tree = git(root, &["write-tree"]);
    assert_eq!(tree.len(), 40);
    assert!(!root.join(".git/index.lock").exists());
}

#[test]
fn rit_add_stages_a_tracked_deletion() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::remove_file(root.join("hello.txt")).unwrap();
    Repository::discover(root)
        .unwrap()
        .add(&["hello.txt".into()])
        .unwrap();
    assert_eq!(
        git(root, &["diff", "--cached", "--name-status"]),
        "D\thello.txt"
    );
}

#[test]
fn recursive_add_honors_root_and_nested_ignore_rules() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/generated")).unwrap();
    std::fs::write(root.join(".gitignore"), b"*.log\nbuild/\n").unwrap();
    std::fs::write(root.join("src/.gitignore"), b"generated/\n!keep.log\n").unwrap();
    std::fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(root.join("src/debug.log"), b"ignored\n").unwrap();
    std::fs::write(root.join("src/keep.log"), b"included\n").unwrap();
    std::fs::write(root.join("src/generated/code.rs"), b"ignored\n").unwrap();
    std::fs::create_dir(root.join("build")).unwrap();
    std::fs::write(root.join("build/output.bin"), b"ignored\n").unwrap();

    let count = Repository::discover(root)
        .unwrap()
        .add(&[".".into()])
        .unwrap();
    assert!(count >= 3);
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        ".gitignore\nsrc/.gitignore\nsrc/keep.log\nsrc/main.rs"
    );
}

#[test]
fn rit_write_tree_is_byte_identical_to_git_write_tree() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::create_dir_all(root.join("deep/a")).unwrap();
    std::fs::write(root.join("deep/a/file.txt"), b"recursive tree\n").unwrap();
    let repo = Repository::discover(root).unwrap();
    repo.add(&[".".into()]).unwrap();
    assert_eq!(
        repo.write_tree().unwrap().to_hex(),
        git(root, &["write-tree"])
    );
}

#[test]
fn rit_commit_writes_git_compatible_commit_and_advances_head_with_cas() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    let old = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("hello.txt"), b"committed by rit\n").unwrap();
    let repo = Repository::discover(root).unwrap();
    repo.add(&["hello.txt".into()]).unwrap();
    let signature =
        Signature::parse("Rit Author <rit@example.com> 1700000000 +0000", "commit").unwrap();
    let oid = repo
        .commit_index("rit local authoring\n", signature.clone(), signature)
        .unwrap();

    assert_eq!(git(root, &["rev-parse", "HEAD"]), oid.to_hex());
    assert_eq!(git(root, &["rev-parse", "HEAD^"]), old);
    assert_eq!(
        git(root, &["show", "-s", "--format=%s", "HEAD"]),
        "rit local authoring"
    );
    assert_eq!(
        repo.commit(&oid).unwrap().tree.to_hex(),
        git(root, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        git_bytes(root, &["show", "HEAD:hello.txt"]),
        b"committed by rit\n"
    );
}

#[test]
fn restore_replaces_tracked_content_from_index_without_changing_index() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    let before_index = std::fs::read(root.join(".git/index")).unwrap();
    std::fs::write(root.join("hello.txt"), b"local damage\n").unwrap();
    let restored = Repository::discover(root)
        .unwrap()
        .restore_index(&["hello.txt".into()])
        .unwrap();
    assert_eq!(restored, 1);
    assert_eq!(
        std::fs::read(root.join("hello.txt")).unwrap(),
        b"Hello, Forge!\n"
    );
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        before_index
    );
    assert!(
        !std::fs::read_dir(root).unwrap().flatten().any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".rit-checkout-"))
    );
}

#[test]
fn checkout_tree_materializes_snapshot_and_synchronizes_index_without_moving_head() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    let old_head = git(root, &["rev-parse", "HEAD"]);
    let old_tree = ObjectId::from_hex(&git(root, &["rev-parse", "HEAD^{tree}"])).unwrap();
    std::fs::write(root.join("hello.txt"), b"new head\n").unwrap();
    std::fs::write(root.join("added.txt"), b"remove on checkout\n").unwrap();
    commit_all(root, "new head");
    let new_head = git(root, &["rev-parse", "HEAD"]);

    let written = Repository::discover(root)
        .unwrap()
        .checkout_tree(old_tree)
        .unwrap();
    assert_eq!(written, 1);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), new_head);
    assert_ne!(new_head, old_head);
    assert_eq!(git(root, &["write-tree"]), old_tree.to_hex());
    assert_eq!(
        std::fs::read(root.join("hello.txt")).unwrap(),
        b"Hello, Forge!\n"
    );
    assert!(!root.join("added.txt").exists());
    assert_eq!(git(root, &["diff", "--name-only"]), "");
}

#[test]
fn checkout_tree_rejects_modified_and_untracked_collisions_before_mutation() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::write(root.join("hello.txt"), b"target tracked version\n").unwrap();
    std::fs::write(root.join("future.txt"), b"target version\n").unwrap();
    commit_all(root, "future target");
    let target_tree = ObjectId::from_hex(&git(root, &["rev-parse", "HEAD^{tree}"])).unwrap();
    git(root, &["reset", "--hard", "HEAD^"]);
    let before_index = std::fs::read(root.join(".git/index")).unwrap();
    std::fs::write(root.join("future.txt"), b"untracked version\n").unwrap();

    let collision = Repository::discover(root)
        .unwrap()
        .checkout_tree(target_tree);
    assert!(matches!(collision, Err(ForgeError::WorktreeCollision(_))));
    assert_eq!(
        std::fs::read(root.join("future.txt")).unwrap(),
        b"untracked version\n"
    );
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        before_index
    );

    std::fs::remove_file(root.join("future.txt")).unwrap();
    std::fs::write(root.join("hello.txt"), b"local tracked edit\n").unwrap();
    let modified = Repository::discover(root)
        .unwrap()
        .checkout_tree(target_tree);
    assert!(matches!(modified, Err(ForgeError::WorktreeCollision(_))));
    assert_eq!(
        std::fs::read(root.join("hello.txt")).unwrap(),
        b"local tracked edit\n"
    );
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        before_index
    );
}

#[cfg(unix)]
#[test]
fn restore_never_follows_a_symlinked_parent_outside_the_worktree() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_file(root.join("sub/nested.txt")).unwrap();
    std::fs::remove_dir(root.join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("sub")).unwrap();

    let result = Repository::discover(root)
        .unwrap()
        .restore_index(&["sub".into()]);
    assert!(matches!(result, Err(ForgeError::UnsafePath(_))));
    assert!(!outside.path().join("nested.txt").exists());
}

#[test]
fn existing_index_lock_blocks_without_losing_the_index() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    let index_path = root.join(".git/index");
    let before = std::fs::read(&index_path).unwrap();
    std::fs::write(root.join("blocked.txt"), b"must not stage\n").unwrap();
    std::fs::write(root.join(".git/index.lock"), b"other writer").unwrap();

    let result = Repository::discover(root)
        .unwrap()
        .add(&["blocked.txt".into()]);
    assert!(matches!(result, Err(ForgeError::IndexLocked(_))));
    assert_eq!(std::fs::read(index_path).unwrap(), before);
    assert_eq!(
        std::fs::read(root.join(".git/index.lock")).unwrap(),
        b"other writer"
    );
}

#[test]
fn concurrent_index_writer_is_rejected_without_lost_update() {
    let dir = init_repo_with_commit();
    let root = dir.path();
    std::fs::write(root.join("racer.txt"), b"racing writer\n").unwrap();
    let git_dir = root.join(".git");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer_git_dir = git_dir.clone();
    let writer = std::thread::spawn(move || {
        Index::mutate(&writer_git_dir, |_: &mut Index| {
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    locked_rx.recv().unwrap();

    let competing = Repository::discover(root)
        .unwrap()
        .add(&["racer.txt".into()]);
    assert!(matches!(competing, Err(ForgeError::IndexLocked(_))));
    release_tx.send(()).unwrap();
    writer.join().unwrap().unwrap();

    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    assert!(!git_dir.join("index.lock").exists());
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

/// A packfile generated natively by `rit` (no `git upload-pack`) must be a valid
/// git packfile: `git unpack-objects` decodes every object, and the resulting
/// repository `fsck`s clean.
#[test]
fn rit_generated_pack_is_decoded_by_stock_git() {
    let source = init_repo_with_commit();
    let repo = rit::Repository::open_git_dir(source.path().join(".git")).unwrap();
    let head = repo.head().unwrap();

    // Object set reachable from HEAD (commit + trees + blobs), packed by rit.
    let send = rit::reachable_objects(&repo, &[head], &[]).unwrap();
    assert!(send.len() >= 4, "expected commit+trees+blobs, got {}", send.len());
    let pack = rit::generate_pack(&repo, &send).unwrap();
    assert_eq!(&pack[0..4], b"PACK");

    // Feed the pack to stock git in a fresh repo.
    let target = tempfile::tempdir().unwrap();
    git(target.path(), &["init", "-q"]);
    let mut child = Command::new("git")
        .arg("unpack-objects")
        .arg("-q")
        .current_dir(target.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&pack).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git unpack-objects rejected rit's pack: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Every object is now present and valid.
    assert_eq!(git(target.path(), &["cat-file", "-t", &head.to_hex()]), "commit");
    git(target.path(), &["fsck", "--strict"]);
}

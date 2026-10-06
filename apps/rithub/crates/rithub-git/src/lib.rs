//! # rithub-git
//!
//! The **git-hosting layer**: it creates bare repositories on disk with `rit`,
//! seeds/accepts commits, and produces the byte streams a stock `git` client
//! fetches over the **dumb HTTP protocol**. This is the *only* rithub crate that
//! depends on `rit` — the entire git dependency is isolated here.
//!
//! ## Dumb HTTP hosting, end to end
//!
//! A dumb `git clone http://host/<org>/<repo>.git` performs, in order:
//!
//! 1. `GET .../info/refs` — a plain `"<oid>\t<ref>\n"` advertisement, produced
//!    by [`rit::dumb_info_refs`] via [`RepoHost::info_refs`].
//! 2. `GET .../HEAD` — the symbolic default-branch pointer ([`RepoHost::head`]).
//! 3. `GET .../objects/<xx>/<rest>` — each loose object, served **raw** (still
//!    zlib-compressed exactly as stored) by [`RepoHost::loose_object`]; the
//!    client parses each object and walks to the ones it references.
//! 4. `GET .../objects/info/packs` and `.../objects/pack/<file>` — any packs
//!    ([`RepoHost::objects_info_packs`], [`RepoHost::pack_file`]).
//!
//! Because `rit` writes standard loose objects and standard refs, this walk
//! reconstructs a byte-identical repository on the client — which is the proof
//! that rithub genuinely *hosts* git.

use std::path::{Path, PathBuf};

use std::collections::HashSet;

use rit::object::ObjectType;
use rit::tree::TreeEntry;
use rit::{
    advertise_receive_pack, encode_packet_line, encode_receive_status, execute_receive_pack,
    parse_receive_pack_request, PacketLine, PacketLineLimits, ReceiveLimits, ReceivePackPolicy,
    ReceiveStatus, Repository, Signature, Tree, RECEIVE_PACK_CAPABILITIES,
};

pub mod browse;
pub mod collab;
pub mod error;
pub mod wiki;
pub use browse::{BlobContent, TreeItem};
pub use collab::{
    CommitInfo, DiffResult, FileDiff, MergeAnalysis, MergeResult, MergeStatus, MergeStrategy,
};
pub use error::{GitHostError, Result};
/// Re-exported so forge crates can name commit ids without depending on `rit`.
pub use rit::ObjectId;

/// Server-side push policy for a receive-pack request: which branches block a
/// direct push, and whether *this* pusher is allowed to bypass that (i.e. is an
/// admin). Force-pushes (non-fast-forward) are rejected independently by `rit`'s
/// default receive policy.
#[derive(Clone, Debug, Default)]
pub struct PushPolicy {
    /// Branch names (without the `refs/heads/` prefix) that block direct pushes.
    pub protected_branches: HashSet<String>,
    /// Whether the pusher may push to a protected branch (admin+).
    pub can_push_protected: bool,
}

/// Outcome of a smart-HTTP receive-pack (push) request.
pub struct ReceivePackOutcome {
    /// The report-status pkt-line bytes to return to the client (carries the
    /// per-ref `ok`/`ng` result the git client interprets).
    pub response: Vec<u8>,
    /// True when at least one ref was actually updated or deleted, so the caller
    /// can refresh dumb server-info only when something changed.
    pub changed: bool,
    /// True when the push was rejected (protected branch or ref policy).
    pub rejected: bool,
    /// A short, privacy-safe rejection reason (for the audit log).
    pub reason: Option<String>,
    /// The ref names the request tried to update (for the audit log).
    pub ref_updates: Vec<String>,
}

/// A single flat file to seed into a repository's root tree.
pub struct SeedFile {
    /// The file name (a single path component — no `/`).
    pub name: String,
    /// The file contents.
    pub content: Vec<u8>,
    /// Whether the file is executable (mode `100755` vs `100644`).
    pub executable: bool,
}

/// Hosts bare git repositories rooted at a durable directory. Repositories live
/// at `<root>/<org>/<repo>.git`.
#[derive(Clone)]
pub struct RepoHost {
    root: PathBuf,
}

impl RepoHost {
    /// Create a host rooted at `root` (created on demand).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The on-disk bare-repo path for `<org>/<repo>` (does not touch disk).
    pub fn repo_path(&self, org: &str, repo: &str) -> Result<PathBuf> {
        validate_component("organization", org)?;
        validate_component("repository", repo)?;
        Ok(self.root.join(org).join(format!("{repo}.git")))
    }

    /// Whether the bare repository already exists on disk.
    pub fn exists(&self, org: &str, repo: &str) -> Result<bool> {
        Ok(self.repo_path(org, repo)?.join("HEAD").exists())
    }

    /// Initialize a new **bare** repository on disk (via `rit`), defaulting HEAD
    /// to `refs/heads/<default_branch>`. Idempotent: re-init preserves an
    /// existing HEAD/config. Also writes the dumb-HTTP server-info files.
    pub fn init_bare(&self, org: &str, repo: &str, default_branch: &str) -> Result<PathBuf> {
        let path = self.repo_path(org, repo)?;
        std::fs::create_dir_all(&path)?;
        let repository = Repository::init(&path, true, Some(default_branch))?;
        rit::update_server_info(&repository)?;
        Ok(path)
    }

    /// Open an existing bare repository.
    pub fn open(&self, org: &str, repo: &str) -> Result<Repository> {
        let path = self.repo_path(org, repo)?;
        Ok(Repository::open_git_dir(&path)?)
    }

    /// Seed a commit consisting of `files` at the repository root, advancing the
    /// given branch. Builds real blob/tree/commit objects through `rit` and
    /// updates `refs/heads/<branch>` with an atomic compare-and-swap, so this
    /// works on a freshly-initialized bare repo and is safe under concurrency.
    ///
    /// Returns the new commit's hex oid.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_commit(
        &self,
        org: &str,
        repo: &str,
        branch: &str,
        files: &[SeedFile],
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        if files.is_empty() {
            return Err(GitHostError::Invalid("seed requires at least one file".into()));
        }
        let repository = self.open(org, repo)?;

        // 1. Write each file as a blob and collect tree entries.
        let mut entries = Vec::with_capacity(files.len());
        for file in files {
            if file.name.is_empty() || file.name.contains('/') || file.name.contains('\0') {
                return Err(GitHostError::Invalid(format!(
                    "seed file name must be a single path component: {:?}",
                    file.name
                )));
            }
            let oid = repository.hash_object(ObjectType::Blob, &file.content, true)?;
            let mode = if file.executable { "100755" } else { "100644" };
            entries.push(TreeEntry {
                mode: mode.into(),
                name: file.name.clone().into_bytes(),
                oid,
            });
        }

        // 2. Build the canonical root tree.
        let mut tree = Tree { entries };
        tree.canonicalize();
        let tree_oid = repository.hash_object(ObjectType::Tree, &tree.serialize(), true)?;

        // 3. Commit onto the branch (parent = current tip, if any).
        let branch_ref = format!("refs/heads/{branch}");
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let signature = Signature::parse(
            &format!("{author_name} <{author_email}> {seconds} +0000"),
            "commit",
        )?;
        let parents = match repository.refs().resolve(&branch_ref) {
            Ok(oid) => vec![oid],
            Err(_) => Vec::new(),
        };
        let commit = repository.commit_tree(
            tree_oid,
            parents,
            signature.clone(),
            signature,
            message.as_bytes(),
            Some(&branch_ref),
        )?;

        // 4. Refresh the on-disk dumb-HTTP advertisement.
        rit::update_server_info(&repository)?;
        Ok(commit.to_hex())
    }

    // --- dumb HTTP serving ------------------------------------------------

    /// The dumb `info/refs` advertisement (`"<oid>\t<ref>\n"` lines).
    pub fn info_refs(&self, org: &str, repo: &str) -> Result<String> {
        Ok(rit::dumb_info_refs(&self.open(org, repo)?)?)
    }

    /// The raw `HEAD` file bytes (e.g. `ref: refs/heads/main\n`).
    pub fn head(&self, org: &str, repo: &str) -> Result<Vec<u8>> {
        let path = self.repo_path(org, repo)?.join("HEAD");
        Ok(std::fs::read(path)?)
    }

    /// The `objects/info/packs` listing.
    pub fn objects_info_packs(&self, org: &str, repo: &str) -> Result<String> {
        Ok(rit::objects_info_packs(&self.open(org, repo)?)?)
    }

    /// The **raw** (still zlib-compressed) bytes of one loose object, addressed
    /// by its two-char shard and 38-char suffix. Both parts are validated as hex
    /// so a request can never escape the `objects` directory.
    pub fn loose_object(&self, org: &str, repo: &str, shard: &str, rest: &str) -> Result<Vec<u8>> {
        if shard.len() != 2 || !is_hex(shard) || rest.len() != 38 || !is_hex(rest) {
            return Err(GitHostError::Invalid(format!(
                "not a loose object path: {shard}/{rest}"
            )));
        }
        let path = self
            .repo_path(org, repo)?
            .join("objects")
            .join(shard)
            .join(rest);
        read_no_symlink(&path)
    }

    /// The raw bytes of a pack-directory file (`*.pack` / `*.idx`). The file
    /// name is validated against the canonical `pack-<40hex>.<ext>` shape to
    /// prevent traversal.
    pub fn pack_file(&self, org: &str, repo: &str, file: &str) -> Result<Vec<u8>> {
        if !is_valid_pack_name(file) {
            return Err(GitHostError::Invalid(format!("not a pack file: {file}")));
        }
        let path = self
            .repo_path(org, repo)?
            .join("objects")
            .join("pack")
            .join(file);
        read_no_symlink(&path)
    }

    // --- forks + cross-repo import ---------------------------------------

    /// Fork `<src_org>/<src_repo>` into a new bare repo `<dst_org>/<dst_repo>` by
    /// copying every reachable object and every ref (an independent copy, not an
    /// alternates link). Returns the new repo's path.
    pub fn fork_repo(
        &self,
        src_org: &str,
        src_repo: &str,
        dst_org: &str,
        dst_repo: &str,
    ) -> Result<std::path::PathBuf> {
        let src = self.open(src_org, src_repo)?;
        let default_branch = self.default_branch(src_org, src_repo).unwrap_or_else(|_| "main".into());
        let path = self.init_bare(dst_org, dst_repo, &default_branch)?;
        let dst = self.open(dst_org, dst_repo)?;

        let refs = src.refs().list()?;
        let tips: Vec<ObjectId> = refs.iter().map(|(_, oid)| *oid).collect();
        // Copy the full object closure of all refs, then reproduce the refs.
        for oid in rit::reachable_objects(&src, &tips, &[])? {
            let (object_type, payload) = src.odb().read_raw(&oid)?;
            dst.odb().write(object_type, &payload)?;
        }
        for (name, oid) in refs {
            let expected = dst.refs().resolve(&name).ok();
            dst.refs()
                .compare_and_swap(&name, expected, oid)
                .map_err(GitHostError::Rit)?;
        }
        let _ = rit::update_server_info(&dst);
        Ok(path)
    }

    /// Import the object closure of `commit` from `<from_org>/<from_repo>` into
    /// `<into_org>/<into_repo>` and point `pull_ref` (e.g. `refs/pull/3/head`) at
    /// it. This is how a **cross-repo** PR makes a fork's head resolvable and
    /// mergeable inside the upstream repo.
    pub fn import_commit(
        &self,
        from_org: &str,
        from_repo: &str,
        into_org: &str,
        into_repo: &str,
        commit: ObjectId,
        pull_ref: &str,
    ) -> Result<()> {
        let from = self.open(from_org, from_repo)?;
        let into = self.open(into_org, into_repo)?;
        for oid in rit::reachable_objects(&from, &[commit], &[])? {
            let (object_type, payload) = from.odb().read_raw(&oid)?;
            into.odb().write(object_type, &payload)?;
        }
        let expected = into.refs().resolve(pull_ref).ok();
        into.refs()
            .compare_and_swap(pull_ref, expected, commit)
            .map_err(GitHostError::Rit)?;
        let _ = rit::update_server_info(&into);
        Ok(())
    }

    // --- smart HTTP: fetch (upload-pack) ----------------------------------

    /// The smart-HTTP `GET info/refs?service=git-upload-pack` body: the
    /// `# service` banner followed by **`rit`'s native** v0 ref advertisement
    /// (no `git upload-pack` bridge). `git_protocol` (the client's `Git-Protocol`
    /// header) is accepted for signature compatibility but not needed — `rit`
    /// serves protocol v0 and modern clients degrade gracefully.
    pub fn upload_pack_advertise(
        &self,
        org: &str,
        repo: &str,
        _git_protocol: Option<&str>,
    ) -> Result<Vec<u8>> {
        let repository = self.open(org, repo)?;
        let mut body = service_banner("git-upload-pack")?;
        body.extend(rit::advertise_upload_pack(&repository)?);
        Ok(body)
    }

    /// A smart-HTTP `POST git-upload-pack` negotiation round, served by **`rit`'s
    /// native** upload-pack: parse the want/have request, compute the object set,
    /// and return `NAK` + a `rit`-generated packfile.
    pub fn upload_pack_rpc(
        &self,
        org: &str,
        repo: &str,
        body: &[u8],
        _git_protocol: Option<&str>,
    ) -> Result<Vec<u8>> {
        let repository = self.open(org, repo)?;
        Ok(rit::upload_pack_v0(&repository, body)?)
    }

    // --- smart HTTP: push (receive-pack) ----------------------------------

    /// The smart-HTTP `GET info/refs?service=git-receive-pack` body: the
    /// `# service` banner followed by `rit`'s native receive-pack ref
    /// advertisement (the first ref carries the server capabilities).
    pub fn receive_pack_advertise(&self, org: &str, repo: &str) -> Result<Vec<u8>> {
        let repository = self.open(org, repo)?;
        let mut body = service_banner("git-receive-pack")?;
        body.extend(advertise_receive_pack(&repository, RECEIVE_PACK_CAPABILITIES)?);
        Ok(body)
    }

    /// Apply a smart-HTTP `POST git-receive-pack` request through `rit`'s native
    /// receiver, first enforcing the server-side [`PushPolicy`].
    ///
    /// **Protected-branch enforcement happens here, at receive-pack** — before
    /// any objects are published or any ref is moved: a direct push to a
    /// protected branch by a non-admin is rejected with a clear per-ref `ng`
    /// message and the pushed pack is discarded (never stored). Non-protected
    /// branches proceed to `rit`, where new-branch and fast-forward updates
    /// succeed and a non-fast-forward (force) push is rejected by the default
    /// receive policy. On any real ref change the dumb server-info is refreshed.
    pub fn receive_pack(
        &self,
        org: &str,
        repo: &str,
        body: &[u8],
        policy: &PushPolicy,
    ) -> Result<ReceivePackOutcome> {
        let repository = self.open(org, repo)?;

        // Parse commands up front so protection is checked before execution.
        let request =
            parse_receive_pack_request(body, RECEIVE_PACK_CAPABILITIES, PacketLineLimits::default())?;
        let ref_updates: Vec<String> =
            request.commands.iter().map(|c| c.name().to_string()).collect();

        let blocked: Vec<String> = request
            .commands
            .iter()
            .filter_map(|cmd| cmd.name().strip_prefix("refs/heads/").map(|b| (cmd.name(), b)))
            .filter(|(_, branch)| {
                policy.protected_branches.contains(*branch) && !policy.can_push_protected
            })
            .map(|(name, _)| name.to_string())
            .collect();

        if !blocked.is_empty() {
            let report_status = request.capabilities.contains("report-status")
                || request.capabilities.contains("report-status-v2");
            let response = if report_status {
                let statuses: Vec<ReceiveStatus> = request
                    .commands
                    .iter()
                    .map(|cmd| ReceiveStatus {
                        ref_name: cmd.name().to_string(),
                        error: Some(
                            "protected branch: direct push blocked, open a pull request".into(),
                        ),
                    })
                    .collect();
                encode_receive_status(None, &statuses)?
            } else {
                Vec::new()
            };
            return Ok(ReceivePackOutcome {
                response,
                changed: false,
                rejected: true,
                reason: Some("protected branch".into()),
                ref_updates,
            });
        }

        let execution = execute_receive_pack(
            &repository,
            body,
            RECEIVE_PACK_CAPABILITIES,
            PacketLineLimits::default(),
            ReceiveLimits::default(),
            ReceivePackPolicy::default(), // non-fast-forward rejected by default
        )?;
        let changed = execution.outcome.is_some() || execution.deleted.is_some();
        if changed {
            // Best-effort refresh; a stale dumb advertisement must not fail an
            // otherwise-successful push.
            let _ = rit::update_server_info(&repository);
        }
        let (rejected, reason) = match &execution.rejection {
            Some(_) => (true, Some("rejected by ref policy (e.g. non-fast-forward)".into())),
            None => (false, None),
        };
        Ok(ReceivePackOutcome {
            response: execution.response,
            changed,
            rejected,
            reason,
            ref_updates,
        })
    }
}

/// Build the `# service=<name>\n` pkt-line banner + flush that prefixes every
/// smart-HTTP `info/refs` response.
fn service_banner(service: &str) -> Result<Vec<u8>> {
    let mut out = encode_packet_line(&PacketLine::Data(
        format!("# service={service}\n").into_bytes(),
    ))?;
    out.extend_from_slice(b"0000");
    Ok(out)
}

/// Read a file, refusing to follow a symlink (defense against a repo dir that
/// was tampered with to point outside itself).
fn read_no_symlink(path: &Path) -> Result<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(GitHostError::Invalid("refusing to serve a symlink".into()))
        }
        Ok(_) => Ok(std::fs::read(path)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(GitHostError::NotFound(path.display().to_string()))
        }
        Err(e) => Err(e.into()),
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Canonical pack file name: `pack-<40 hex>.pack` or `pack-<40 hex>.idx`.
fn is_valid_pack_name(file: &str) -> bool {
    let stem = match file
        .strip_suffix(".pack")
        .or_else(|| file.strip_suffix(".idx"))
    {
        Some(stem) => stem,
        None => return false,
    };
    match stem.strip_prefix("pack-") {
        Some(hex) => hex.len() == 40 && is_hex(hex),
        None => false,
    }
}

/// Validate a single path component (org/repo directory name). Mirrors the
/// forge-domain rules but re-checked here so `rithub-git` is safe standalone.
fn validate_component(kind: &'static str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    if ok {
        Ok(())
    } else {
        Err(GitHostError::Invalid(format!("invalid {kind} name: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_seed_and_serve_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let host = RepoHost::new(dir.path());
        host.init_bare("acme", "widgets", "main").unwrap();
        assert!(host.exists("acme", "widgets").unwrap());

        let oid = host
            .seed_commit(
                "acme",
                "widgets",
                "main",
                &[SeedFile {
                    name: "README.md".into(),
                    content: b"# widgets\n".to_vec(),
                    executable: false,
                }],
                "seed commit\n",
                "Ada",
                "ada@example.com",
            )
            .unwrap();
        assert_eq!(oid.len(), 40);

        let advertised = host.info_refs("acme", "widgets").unwrap();
        assert_eq!(advertised, format!("{oid}\trefs/heads/main\n"));

        let head = host.head("acme", "widgets").unwrap();
        assert_eq!(head, b"ref: refs/heads/main\n");

        // The commit object is served raw and is non-empty.
        let shard = &oid[..2];
        let rest = &oid[2..];
        let raw = host.loose_object("acme", "widgets", shard, rest).unwrap();
        assert!(!raw.is_empty());
    }

    #[test]
    fn rejects_path_traversal_components() {
        let dir = tempfile::tempdir().unwrap();
        let host = RepoHost::new(dir.path());
        assert!(host.repo_path("..", "x").is_err());
        assert!(host.repo_path("org", "..").is_err());
        assert!(host.loose_object("org", "repo", "..", "x").is_err());
        assert!(host.pack_file("org", "repo", "../../etc/passwd").is_err());
    }
}

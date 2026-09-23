//! `rit` — a compatibility-oriented CLI and receive-pack process server.
//!
//! Runnable proof of the library: `hash-object`, `cat-file`, `ls-tree`, and
//! `log`, operating on real `.git` repositories.

use std::io::{Read, Write};
use std::path::PathBuf;

mod forge_cli;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use rit::object::Object;
use rit::{
    abort_merge, advertise_receive_pack, cmd_cherry_pick, cmd_diff, cmd_merge, cmd_rebase,
    cmd_sequencer_abort, cmd_sequencer_continue, cmd_sequencer_skip, cmd_show, cmd_stash_apply,
    cmd_stash_drop, cmd_stash_pop, cmd_stash_push, cmd_switch, continue_merge, dispatch_branch,
    dispatch_tag, execute_receive_pack, parse_receive_pack_request, smart_http_serve_blocking,
    stash_list, BranchOp, MergeKind, ObjectType, PacketLineLimits, ReceiveLimits,
    ReceivePackCommand, ReceivePackPolicy, Repository, SequencerStatus, Signature, SmartHttpConfig,
    SmartHttpState, StashApply, SwitchHead, TagOp, WorktreeState, RECEIVE_PACK_CAPABILITIES,
};

#[derive(Parser)]
#[command(
    name = "rit",
    version,
    about = "rithub: inspect and create Git objects (loose-object core slice)"
)]
struct Cli {
    /// Repository path to operate on (its `.git` is discovered by walking up).
    #[arg(short = 'C', long, default_value = ".", global = true)]
    repo: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty Git repository or reinitialize an existing one. Like `git init`.
    Init(InitArgs),
    /// List references and the object ids they resolve to. Like `git show-ref`.
    ShowRef,
    /// Compute an object id (and optionally write the object). Like `git hash-object`.
    HashObject(HashObjectArgs),
    /// Print an object's type or pretty content. Like `git cat-file`.
    CatFile(CatFileArgs),
    /// List the entries of a tree. Like `git ls-tree`.
    LsTree(LsTreeArgs),
    /// Show commit history from a revision (default HEAD). Like `git log`.
    Log(LogArgs),
    /// Show tracked worktree changes from the checksum-verified Git index.
    Status,
    /// Stage explicit files or tracked deletions in the Git index.
    Add(AddArgs),
    /// Write the staged index as a recursive tree object.
    WriteTree,
    /// Commit the staged index and atomically advance the current branch.
    Commit(CommitArgs),
    /// Safely materialize a commit/tree snapshot and synchronize the index without moving HEAD.
    CheckoutTree(CheckoutTreeArgs),
    /// List, create, delete, or rename local branches. Like `git branch`.
    Branch(BranchArgs),
    /// List, create, or delete tags (lightweight or annotated). Like `git tag`.
    Tag(TagArgs),
    /// Switch branches or restore a detached HEAD, updating the worktree. Like `git switch`.
    Switch(SwitchArgs),
    /// Switch to a branch or commit, optionally creating it. Like `git checkout`.
    Checkout(CheckoutArgs),
    /// Show changes between commits, the index, and the worktree. Like `git diff`.
    Diff(DiffArgs),
    /// Show an object (commit, tag, tree, or blob) with its diff. Like `git show`.
    Show(ShowArgs),
    /// Restore tracked paths from the current index.
    Restore(RestoreArgs),
    /// Serve one stateful Git receive-pack session over stdin/stdout.
    ReceivePack(ReceivePackArgs),
    /// Serve Git upload-pack through the installed open-source Git backend.
    UploadPack(UploadPackArgs),
    /// Join another branch's history into the current one. Like `git merge`.
    Merge(MergeArgs),
    /// Clone a repository from a rithub/smart-HTTP URL or a local path. Like `git clone`.
    Clone(CloneArgs),
    /// Manage configured remotes. Like `git remote`.
    Remote(RemoteArgs),
    /// Download branches from a remote into `refs/remotes/<remote>/*`. Like `git fetch`.
    Fetch(FetchArgs),
    /// Fetch the current branch's upstream and fast-forward to it. Like `git pull --ff-only`.
    Pull(PullArgs),
    /// Upload a branch to a remote. Like `git push`.
    Push(PushArgs),
    /// Reapply commits on top of another base tip. Like `git rebase`.
    Rebase(RebaseArgs),
    /// Apply the changes of existing commits. Like `git cherry-pick`.
    CherryPick(CherryPickArgs),
    /// Stash the dirty worktree/index away and restore it later. Like `git stash`.
    Stash(StashArgs),
    /// Serve one repository over the Git smart-HTTP protocol.
    ServeHttp(ServeHttpArgs),
    /// Work with rithub issues: list, view, create, close, reopen, comment.
    Issue(forge_cli::IssueArgs),
    /// Work with rithub pull requests: list, view, create, merge, close,
    /// reopen, comment, diff, checks.
    Pr(forge_cli::PrArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Directory to initialize (created if needed). Defaults to the current directory.
    #[arg(default_value = ".")]
    directory: PathBuf,
    /// Create a bare repository (no worktree; skeleton lives directly in `directory`).
    #[arg(long)]
    bare: bool,
    /// Name of the initial branch HEAD points at (default: main).
    #[arg(short = 'b', long = "initial-branch")]
    branch: Option<String>,
}

#[derive(Args)]
struct HashObjectArgs {
    /// Object type to hash as.
    #[arg(short = 't', long = "type", default_value = "blob")]
    object_type: String,
    /// Write the object into the object database.
    #[arg(short = 'w')]
    write: bool,
    /// Read the content from stdin instead of a file.
    #[arg(long)]
    stdin: bool,
    /// Path to the file to hash (unless `--stdin`).
    file: Option<PathBuf>,
}

#[derive(Args)]
struct CatFileArgs {
    /// Show the object type.
    #[arg(short = 't', group = "mode")]
    show_type: bool,
    /// Pretty-print the object content.
    #[arg(short = 'p', group = "mode")]
    pretty: bool,
    /// The revision / object to read (oid, ref, or HEAD).
    object: String,
}

#[derive(Args)]
struct LsTreeArgs {
    /// A revision resolving to a tree or a commit (whose tree is listed).
    #[arg(default_value = "HEAD")]
    rev: String,
}

#[derive(Args)]
struct LogArgs {
    /// Revision to start from.
    #[arg(default_value = "HEAD")]
    rev: String,
    /// Maximum number of commits to show.
    #[arg(short = 'n', long)]
    max: Option<usize>,
}

#[derive(Args)]
struct AddArgs {
    /// Worktree-relative files or directories to stage recursively.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Args)]
struct CommitArgs {
    /// Commit message.
    #[arg(short = 'm', long, required = true)]
    message: String,
}

#[derive(Args)]
struct CheckoutTreeArgs {
    /// Commit or tree revision to materialize. HEAD is accepted.
    rev: String,
}

#[derive(Args)]
struct RestoreArgs {
    /// Worktree-relative tracked files or directories to restore from the index.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Args)]
struct BranchArgs {
    /// Delete a branch (must be fully merged unless forced). Like `git branch -d`.
    #[arg(short = 'd', long = "delete")]
    delete: bool,
    /// Force-delete a branch even if unmerged. Like `git branch -D`.
    #[arg(short = 'D')]
    delete_force: bool,
    /// Rename a branch: `<old> <new>`. Like `git branch -m`.
    #[arg(short = 'm', long = "move")]
    rename: bool,
    /// Force create/rename over an existing branch. Like `git branch -f`.
    #[arg(short = 'f', long = "force")]
    force: bool,
    /// Show the tip commit id and subject alongside each branch.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
    /// Branch names / start-point. Absent → list; one → create; two → create-at or rename.
    names: Vec<String>,
}

#[derive(Args)]
struct TagArgs {
    /// Create an annotated tag object (requires `-m`). Like `git tag -a`.
    #[arg(short = 'a', long = "annotate")]
    annotate: bool,
    /// Annotation message. Its presence implies an annotated tag.
    #[arg(short = 'm', long = "message")]
    message: Option<String>,
    /// Delete the named tags. Like `git tag -d`.
    #[arg(short = 'd', long = "delete")]
    delete: bool,
    /// Replace an existing tag with the same name. Like `git tag -f`.
    #[arg(short = 'f', long = "force")]
    force: bool,
    /// List tags (default when no name is given). Like `git tag -l`.
    #[arg(short = 'l', long = "list")]
    list: bool,
    /// When listing, print the first line of each tag's annotation. Like `git tag -n`.
    #[arg(short = 'n')]
    show_annotation: bool,
    /// Tag name and optional target revision (defaults to HEAD).
    names: Vec<String>,
}

#[derive(Args)]
struct SwitchArgs {
    /// Create a new branch at the current HEAD before switching. Like `git switch -c`.
    #[arg(short = 'c', long = "create")]
    create: bool,
    /// Branch name, commit, or tag to switch to.
    target: String,
}

#[derive(Args)]
struct CheckoutArgs {
    /// Create a new branch at the current HEAD before switching. Like `git checkout -b`.
    #[arg(short = 'b')]
    create: bool,
    /// Branch name, commit, or tag to check out.
    target: String,
}

#[derive(Args)]
struct DiffArgs {
    /// Compare the staged index against HEAD instead of the worktree.
    #[arg(long = "cached", visible_alias = "staged")]
    cached: bool,
    /// Optional revisions: none (worktree/index), or two trees to compare.
    revs: Vec<String>,
}

#[derive(Args)]
struct ShowArgs {
    /// Object to show (commit, tag, tree, or blob). Defaults to HEAD.
    object: Option<String>,
}

#[derive(Args)]
struct ReceivePackArgs {
    /// Exact bare repository/git directory supplied by Git's transport.
    git_dir: PathBuf,
    /// Branch (short name, e.g. `main`) whose direct push is blocked. Repeatable.
    /// A transport (SSH/HTTP) passes the repo's protected branches here for
    /// non-admin pushers so the same protection applies as over smart-HTTP.
    #[arg(long = "block-branch")]
    block_branch: Vec<String>,
}

#[derive(Args)]
struct UploadPackArgs {
    /// Exact bare repository/git directory supplied by Git's transport.
    git_dir: PathBuf,
}

#[derive(Args)]
struct MergeArgs {
    /// Create a merge commit even when a fast-forward is possible. Like `git merge --no-ff`.
    #[arg(long = "no-ff")]
    no_ff: bool,
    /// Conclude a conflicted merge after resolving the worktree. Like `git merge --continue`.
    #[arg(long = "continue", conflicts_with_all = ["abort", "target"])]
    cont: bool,
    /// Abort an in-progress merge and restore the pre-merge state. Like `git merge --abort`.
    #[arg(long = "abort", conflicts_with_all = ["cont", "target"])]
    abort: bool,
    /// Merge commit message (defaults to a generated `Merge ...` line).
    #[arg(short = 'm', long = "message")]
    message: Option<String>,
    /// Branch, tag, or commit to merge into the current branch.
    target: Option<String>,
}

#[derive(Args)]
struct RebaseArgs {
    /// Reset onto this base instead of `<upstream>`. Like `git rebase --onto`.
    #[arg(long = "onto")]
    onto: Option<String>,
    /// Resume after resolving a conflict. Like `git rebase --continue`.
    #[arg(long = "continue", conflicts_with_all = ["abort", "skip", "upstream"])]
    cont: bool,
    /// Abort and return to the pre-rebase branch tip. Like `git rebase --abort`.
    #[arg(long = "abort", conflicts_with_all = ["cont", "skip", "upstream"])]
    abort: bool,
    /// Drop the current commit and continue. Like `git rebase --skip`.
    #[arg(long = "skip", conflicts_with_all = ["cont", "abort", "upstream"])]
    skip: bool,
    /// Upstream branch/commit whose `..HEAD` commits are replayed.
    upstream: Option<String>,
}

#[derive(Args)]
struct CherryPickArgs {
    /// Resume after resolving a conflict. Like `git cherry-pick --continue`.
    #[arg(long = "continue", conflicts_with_all = ["abort", "skip", "revs"])]
    cont: bool,
    /// Abort the in-progress cherry-pick sequence. Like `git cherry-pick --abort`.
    #[arg(long = "abort", conflicts_with_all = ["cont", "skip", "revs"])]
    abort: bool,
    /// Drop the current commit and continue. Like `git cherry-pick --skip`.
    #[arg(long = "skip", conflicts_with_all = ["cont", "abort", "revs"])]
    skip: bool,
    /// One or more commits to replay onto the current branch.
    revs: Vec<String>,
}

#[derive(Args)]
struct StashArgs {
    #[command(subcommand)]
    action: Option<StashAction>,
}

#[derive(Subcommand)]
enum StashAction {
    /// Record the dirty worktree/index and reset back to HEAD (default action).
    Push {
        /// Optional description for the stash entry.
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
    },
    /// List the stash entries, newest first.
    List,
    /// Apply a stash entry without removing it from the stack.
    Apply {
        /// Stash index (0 = most recent).
        #[arg(default_value_t = 0)]
        index: usize,
    },
    /// Apply a stash entry and drop it on a clean apply.
    Pop {
        /// Stash index (0 = most recent).
        #[arg(default_value_t = 0)]
        index: usize,
    },
    /// Delete a stash entry from the stack.
    Drop {
        /// Stash index (0 = most recent).
        #[arg(default_value_t = 0)]
        index: usize,
    },
}

#[derive(Args)]
struct ServeHttpArgs {
    /// Exact bare repository/git directory to serve at the URL root.
    git_dir: PathBuf,
    /// Address to bind, e.g. `127.0.0.1:8080`.
    #[arg(long = "addr", default_value = "127.0.0.1:8080")]
    addr: String,
    /// Branch (short name) whose direct push is blocked. Repeatable.
    #[arg(long = "block-branch")]
    block_branch: Vec<String>,
    /// Allow non-fast-forward branch updates (a repository-owner decision).
    #[arg(long = "allow-non-ff")]
    allow_non_ff: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("rit: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Init(args) => cmd_init(args),
        Command::ShowRef => cmd_show_ref(&cli),
        Command::HashObject(args) => cmd_hash_object(&cli, args),
        Command::CatFile(args) => cmd_cat_file(&cli, args),
        Command::LsTree(args) => cmd_ls_tree(&cli, args),
        Command::Log(args) => cmd_log(&cli, args),
        Command::Status => cmd_status(&cli),
        Command::Add(args) => cmd_add(&cli, args),
        Command::WriteTree => cmd_write_tree(&cli),
        Command::Commit(args) => cmd_commit(&cli, args),
        Command::CheckoutTree(args) => cmd_checkout_tree(&cli, args),
        Command::Branch(args) => cmd_branch(&cli, args),
        Command::Tag(args) => cmd_tag(&cli, args),
        Command::Switch(args) => cmd_switch_cli(&cli, args),
        Command::Checkout(args) => cmd_checkout(&cli, args),
        Command::Diff(args) => cmd_diff_cli(&cli, args),
        Command::Show(args) => cmd_show_cli(&cli, args),
        Command::Restore(args) => cmd_restore(&cli, args),
        Command::ReceivePack(args) => cmd_receive_pack(args),
        Command::UploadPack(args) => cmd_upload_pack(args),
        Command::Merge(args) => cmd_merge_cli(&cli, args),
        Command::Clone(args) => cmd_clone(args),
        Command::Remote(args) => cmd_remote(&cli, args),
        Command::Fetch(args) => cmd_fetch(&cli, args),
        Command::Pull(args) => cmd_pull(&cli, args),
        Command::Push(args) => cmd_push(&cli, args),
        Command::Rebase(args) => cmd_rebase_cli(&cli, args),
        Command::CherryPick(args) => cmd_cherry_pick_cli(&cli, args),
        Command::Stash(args) => cmd_stash_cli(&cli, args),
        Command::ServeHttp(args) => cmd_serve_http(args),
        Command::Issue(args) => forge_cli::cmd_issue(&cli.repo, args),
        Command::Pr(args) => forge_cli::cmd_pr(&cli.repo, args),
    }
}

fn cmd_merge_cli(cli: &Cli, args: &MergeArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;

    if args.abort {
        abort_merge(&repo)?;
        println!("merge aborted");
        return Ok(());
    }

    let sig = current_signature(repo.git_dir())?;

    if args.cont {
        let oid = continue_merge(&repo, &sig, args.message.as_deref())?;
        println!("{oid}");
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .context("merge requires a target (or --continue/--abort)")?;
    let result = cmd_merge(&repo, target, &sig, args.message.as_deref(), args.no_ff)?;
    match result.kind {
        MergeKind::AlreadyUpToDate => println!("Already up to date."),
        MergeKind::FastForward(oid) => println!("Fast-forward to {oid}"),
        MergeKind::Merged(oid) => println!("Merge made by the three-way strategy: {oid}"),
        MergeKind::Conflicts(paths) => {
            report_conflicts("merge", &paths);
            std::process::exit(1);
        }
    }
    Ok(())
}

fn cmd_rebase_cli(cli: &Cli, args: &RebaseArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;

    if args.abort {
        cmd_sequencer_abort(&repo)?;
        println!("rebase aborted");
        return Ok(());
    }

    let sig = current_signature(repo.git_dir())?;
    let status = if args.cont {
        cmd_sequencer_continue(&repo, &sig)?
    } else if args.skip {
        cmd_sequencer_skip(&repo, &sig)?
    } else {
        let upstream = args
            .upstream
            .as_deref()
            .context("rebase requires an upstream (or --continue/--abort/--skip)")?;
        cmd_rebase(&repo, upstream, args.onto.as_deref(), &sig)?
    };
    report_sequencer("rebase", &status)
}

fn cmd_cherry_pick_cli(cli: &Cli, args: &CherryPickArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;

    if args.abort {
        cmd_sequencer_abort(&repo)?;
        println!("cherry-pick aborted");
        return Ok(());
    }

    let sig = current_signature(repo.git_dir())?;
    let status = if args.cont {
        cmd_sequencer_continue(&repo, &sig)?
    } else if args.skip {
        cmd_sequencer_skip(&repo, &sig)?
    } else {
        if args.revs.is_empty() {
            bail!("cherry-pick requires at least one commit (or --continue/--abort/--skip)");
        }
        let revs: Vec<&str> = args.revs.iter().map(String::as_str).collect();
        cmd_cherry_pick(&repo, &revs, &sig)?
    };
    report_sequencer("cherry-pick", &status)
}

/// Print a sequencer outcome, exiting non-zero when it stopped on a conflict.
fn report_sequencer(op: &str, status: &SequencerStatus) -> Result<()> {
    match status {
        SequencerStatus::Completed => {
            println!("{op} completed");
            Ok(())
        }
        SequencerStatus::Stopped { conflicts } => {
            report_conflicts(op, conflicts);
            std::process::exit(1);
        }
    }
}

fn report_conflicts(op: &str, paths: &[String]) {
    eprintln!("{op} stopped due to conflicts in:");
    for path in paths {
        eprintln!("\t{path}");
    }
    eprintln!("resolve them, then run `rit {op} --continue` (or `--abort`)");
}

fn cmd_stash_cli(cli: &Cli, args: &StashArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;

    match args.action.as_ref() {
        Some(StashAction::List) => {
            for (index, message) in stash_list(&repo)? {
                println!("stash@{{{index}}}: {message}");
            }
            Ok(())
        }
        Some(StashAction::Apply { index }) => report_stash_apply(cmd_stash_apply(&repo, *index)?),
        Some(StashAction::Pop { index }) => report_stash_apply(cmd_stash_pop(&repo, *index)?),
        Some(StashAction::Drop { index }) => {
            cmd_stash_drop(&repo, *index)?;
            println!("dropped stash@{{{index}}}");
            Ok(())
        }
        // `rit stash` with no subcommand defaults to push, like `git stash`.
        Some(StashAction::Push { .. }) | None => {
            let message = match args.action.as_ref() {
                Some(StashAction::Push { message }) => message.as_deref(),
                _ => None,
            };
            let sig = current_signature(repo.git_dir())?;
            match cmd_stash_push(&repo, &sig, message)? {
                Some(oid) => println!("Saved working directory and index state: {oid}"),
                None => println!("No local changes to save"),
            }
            Ok(())
        }
    }
}

fn report_stash_apply(result: StashApply) -> Result<()> {
    match result {
        StashApply::Clean => Ok(()),
        StashApply::Conflicts(paths) => {
            report_conflicts("stash apply", &paths);
            std::process::exit(1);
        }
    }
}

/// Serve one repository over Git smart-HTTP until the process is stopped.
fn cmd_serve_http(args: &ServeHttpArgs) -> Result<()> {
    let addr: std::net::SocketAddr = args
        .addr
        .parse()
        .with_context(|| format!("invalid --addr {:?} (expected ip:port)", args.addr))?;

    // Validate the repository up front so a bad path fails before we bind.
    Repository::open_git_dir(&args.git_dir)
        .with_context(|| format!("opening bare repo at {}", args.git_dir.display()))?;

    let config = SmartHttpConfig {
        blocked_branches: args.block_branch.clone(),
        allow_non_fast_forward: args.allow_non_ff,
        ..SmartHttpConfig::new()
    };
    let state = SmartHttpState::new(args.git_dir.clone(), config);
    eprintln!("rit: serving {} on http://{addr}/", args.git_dir.display());
    smart_http_serve_blocking(addr, state)?;
    Ok(())
}

/// Serve a **native** stateful (SSH/process) upload-pack session over
/// stdin/stdout — no `git upload-pack` backend. Writes the v0 ref advertisement,
/// reads the client's `want`/`have`/`done` negotiation (answering `NAK` after
/// each have batch, since we advertise no multi-ack), then streams `NAK` + a
/// `rit`-generated packfile.
fn cmd_upload_pack(args: &UploadPackArgs) -> Result<()> {
    let repo = Repository::open_git_dir(&args.git_dir)
        .with_context(|| format!("opening bare repo at {}", args.git_dir.display()))?;

    // 1. Advertise refs.
    {
        let advertisement = rit::advertise_upload_pack(&repo)?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&advertisement)?;
        stdout.flush()?;
    }

    // 2. Read the negotiation. The `want` list ends with a flush that needs NO
    //    acknowledgement; only subsequent `have` batches are answered with NAK
    //    (we advertise no multi-ack) until the client sends `done`.
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut wants: Vec<rit::ObjectId> = Vec::new();
    let mut haves: Vec<rit::ObjectId> = Vec::new();
    let mut done = false;

    // Phase 1: the want list, terminated by a flush (or `done`/EOF directly).
    loop {
        match read_pkt_line(&mut input)? {
            PktLine::Want(oid) => wants.push(oid),
            PktLine::Have(oid) => haves.push(oid),
            PktLine::Flush => break,
            PktLine::Done => {
                done = true;
                break;
            }
            PktLine::Eof => {
                done = true;
                break;
            }
            PktLine::Other => {}
        }
    }

    // Phase 2: have-negotiation rounds (only if the client isn't already done).
    while !done {
        let mut batch_had_have = false;
        loop {
            match read_pkt_line(&mut input)? {
                PktLine::Have(oid) => {
                    haves.push(oid);
                    batch_had_have = true;
                }
                PktLine::Done => {
                    done = true;
                    break;
                }
                PktLine::Flush => break,
                PktLine::Eof => {
                    done = true;
                    break;
                }
                PktLine::Want(oid) => wants.push(oid),
                PktLine::Other => {}
            }
        }
        if done {
            break;
        }
        // Acknowledge this have-batch's flush; no common commit → NAK.
        {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"0008NAK\n")?;
            stdout.flush()?;
        }
        if !batch_had_have {
            break; // an empty batch means the client stopped negotiating
        }
    }

    if wants.is_empty() {
        return Ok(());
    }

    // 3. Compute the object set and stream NAK + pack.
    let send = rit::reachable_objects(&repo, &wants, &haves)?;
    let pack = rit::generate_pack(&repo, &send)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(b"0008NAK\n")?;
    stdout.write_all(&pack)?;
    stdout.flush()?;
    Ok(())
}

/// A parsed line from an upload-pack negotiation stream.
enum PktLine {
    Want(rit::ObjectId),
    Have(rit::ObjectId),
    Done,
    Flush,
    Other,
    Eof,
}

/// Read one pkt-line from `input` and classify it for upload-pack negotiation.
fn read_pkt_line(input: &mut impl Read) -> Result<PktLine> {
    let mut header = [0u8; 4];
    match input.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(PktLine::Eof),
        Err(error) => return Err(error.into()),
    }
    let text = std::str::from_utf8(&header).context("pkt-line length not ASCII")?;
    if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("pkt-line length is not hexadecimal");
    }
    let length = usize::from_str_radix(text, 16).context("parsing pkt-line length")?;
    match length {
        0 => Ok(PktLine::Flush),
        1..=3 => Ok(PktLine::Other), // delimiter / response-end control packets
        4 => Ok(PktLine::Other),     // empty payload
        _ => {
            let mut payload = vec![0u8; length - 4];
            input
                .read_exact(&mut payload)
                .context("reading pkt-line payload")?;
            let line = String::from_utf8_lossy(&payload);
            let line = line.trim_end();
            if let Some(rest) = line.strip_prefix("want ") {
                if let Some(hex) = rest.split_whitespace().next() {
                    return Ok(PktLine::Want(rit::ObjectId::from_hex(hex)?));
                }
            } else if let Some(rest) = line.strip_prefix("have ") {
                if let Some(hex) = rest.split_whitespace().next() {
                    if let Ok(oid) = rit::ObjectId::from_hex(hex) {
                        return Ok(PktLine::Have(oid));
                    }
                }
            } else if line == "done" {
                return Ok(PktLine::Done);
            }
            Ok(PktLine::Other)
        }
    }
}

fn cmd_receive_pack(args: &ReceivePackArgs) -> Result<()> {
    let repo = Repository::open_git_dir(&args.git_dir)
        .with_context(|| format!("opening bare repo at {}", args.git_dir.display()))?;
    let advertisement = advertise_receive_pack(&repo, RECEIVE_PACK_CAPABILITIES)?;
    {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&advertisement)?;
        stdout.flush()?;
    }

    let packet_limits = PacketLineLimits::default();
    let receive_limits = ReceiveLimits::default();
    let request = read_receive_pack_request(packet_limits, receive_limits)?;

    // Protected-branch enforcement (same policy as smart-HTTP): if any command
    // targets a blocked branch, reject the whole push with a per-ref `ng` status
    // and never touch objects or refs.
    if !args.block_branch.is_empty() {
        let parsed =
            parse_receive_pack_request(&request, RECEIVE_PACK_CAPABILITIES, packet_limits)?;
        let blocked = parsed.commands.iter().any(|cmd| {
            cmd.name()
                .strip_prefix("refs/heads/")
                .map(|b| args.block_branch.iter().any(|x| x == b))
                .unwrap_or(false)
        });
        if blocked {
            let report_status = parsed.capabilities.contains("report-status")
                || parsed.capabilities.contains("report-status-v2");
            if report_status {
                let statuses: Vec<rit::ReceiveStatus> = parsed
                    .commands
                    .iter()
                    .map(|cmd| rit::ReceiveStatus {
                        ref_name: cmd.name().to_string(),
                        error: Some(
                            "protected branch: direct push blocked, open a pull request".into(),
                        ),
                    })
                    .collect();
                let response = rit::encode_receive_status(None, &statuses)?;
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&response)?;
                stdout.flush()?;
            }
            eprintln!("rit receive-pack rejected: protected branch");
            return Ok(());
        }
    }

    let execution = execute_receive_pack(
        &repo,
        &request,
        RECEIVE_PACK_CAPABILITIES,
        packet_limits,
        receive_limits,
        ReceivePackPolicy::default(),
    )?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&execution.response)?;
    stdout.flush()?;
    if let Some(error) = execution.rejection {
        eprintln!("rit receive-pack rejected request: {error}");
    }
    Ok(())
}

/// Read the command pkt-lines exactly through their flush boundary. A pure
/// deletion has no pack and Git keeps the write side open while awaiting the
/// status response, so waiting for EOF would deadlock. Create/update requests
/// continue through the bounded raw-pack body; stock Git closes that request
/// side after its pack trailer.
fn read_receive_pack_request(
    packet_limits: PacketLineLimits,
    receive_limits: ReceiveLimits,
) -> Result<Vec<u8>> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut request = Vec::new();
    let mut packets = 0usize;
    let mut payload_bytes = 0usize;
    loop {
        if packets == packet_limits.max_packets {
            bail!(
                "receive-pack exceeds {} command packets",
                packet_limits.max_packets
            );
        }
        let mut header = [0u8; 4];
        input
            .read_exact(&mut header)
            .context("reading receive-pack packet header")?;
        let header_text =
            std::str::from_utf8(&header).context("receive-pack packet header is not ASCII")?;
        if !header_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("receive-pack packet length is not hexadecimal");
        }
        let length =
            usize::from_str_radix(header_text, 16).context("parsing receive-pack packet length")?;
        request.extend_from_slice(&header);
        packets += 1;
        match length {
            0 => break,
            1..=3 => bail!("receive-pack command stream used reserved control {header_text}"),
            4..=rit::wire::MAX_PACKET_LENGTH => {
                let payload_len = length - 4;
                payload_bytes = payload_bytes
                    .checked_add(payload_len)
                    .context("receive command byte count overflow")?;
                if payload_bytes > packet_limits.max_total_payload {
                    bail!(
                        "receive commands exceed {} payload bytes",
                        packet_limits.max_total_payload
                    );
                }
                let start = request.len();
                request.resize(start + payload_len, 0);
                input
                    .read_exact(&mut request[start..])
                    .context("reading receive-pack packet payload")?;
            }
            _ => bail!(
                "receive-pack packet length exceeds {} bytes",
                rit::wire::MAX_PACKET_LENGTH
            ),
        }
    }

    let parsed = parse_receive_pack_request(&request, RECEIVE_PACK_CAPABILITIES, packet_limits)?;
    // A pack follows whenever any command creates or updates a ref (including a
    // multi-ref push). A push made up solely of deletions carries no pack, and
    // Git keeps its write side open awaiting status, so reading for EOF there
    // would deadlock.
    let needs_pack = parsed
        .commands
        .iter()
        .any(|command| !matches!(command, ReceivePackCommand::Delete { .. }));
    if needs_pack {
        let max_pack = usize::try_from(receive_limits.max_pack_bytes)
            .context("receive pack limit does not fit this platform")?;
        let read_limit = receive_limits
            .max_pack_bytes
            .checked_add(1)
            .context("receive pack limit overflow")?;
        input
            .take(read_limit)
            .read_to_end(&mut request)
            .context("reading receive-pack pack body")?;
        if request.len().saturating_sub(parsed.pack_offset) > max_pack {
            bail!("receive-pack body exceeds {max_pack} bytes");
        }
    }
    Ok(request)
}

fn cmd_checkout_tree(cli: &Cli, args: &CheckoutTreeArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let oid = repo.resolve(&args.rev)?;
    let tree = match repo.read_object(&oid)? {
        Object::Tree(_) => oid,
        Object::Commit(commit) => commit.tree,
        Object::Tag(_) => repo.peel_to_commit(oid)?.tree,
        Object::Blob(_) => bail!("{} is a blob, not a checkout snapshot", args.rev),
    };
    repo.checkout_tree(tree)?;
    Ok(())
}

fn cmd_restore(cli: &Cli, args: &RestoreArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    repo.restore_index(&args.paths)?;
    Ok(())
}

fn cmd_write_tree(cli: &Cli) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    println!("{}", repo.write_tree()?);
    Ok(())
}

fn cmd_commit(cli: &Cli, args: &CommitArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let signature = current_signature(repo.git_dir())?;
    let oid = repo.commit_index(&args.message, signature.clone(), signature)?;
    println!("{oid}");
    Ok(())
}

/// Build a [`Signature`] for the configured identity stamped at the current
/// wall-clock time in UTC. Shared by `commit`, `branch`, and `tag` handlers.
fn current_signature(git_dir: &std::path::Path) -> Result<Signature> {
    let (name, email) = git_identity(git_dir)?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs();
    let raw = format!("{name} <{email}> {seconds} +0000");
    Ok(Signature::parse(&raw, "commit")?)
}

fn cmd_branch(cli: &Cli, args: &BranchArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let names: Vec<&str> = args.names.iter().map(String::as_str).collect();
    let delete = args.delete || args.delete_force;

    let op = if delete {
        if names.is_empty() {
            bail!("branch -d/-D requires at least one branch name");
        }
        BranchOp::Delete {
            names,
            force: args.delete_force,
        }
    } else if args.rename {
        match names.as_slice() {
            [new] => {
                // `git branch -m <new>` renames the current branch.
                let current = match repo.refs().read_head()? {
                    rit::RefTarget::Symbolic(refname) => refname
                        .strip_prefix("refs/heads/")
                        .unwrap_or(&refname)
                        .to_string(),
                    rit::RefTarget::Direct(_) => {
                        bail!("branch -m with one argument requires being on a branch (HEAD is detached)")
                    }
                };
                let sig = current_signature(repo.git_dir())?;
                return rit::cmd_branch_rename(&repo, &current, new, args.force, &sig);
            }
            [old, new] => BranchOp::Rename {
                old,
                new,
                force: args.force,
            },
            _ => bail!("branch -m requires <old> <new> (or <new> to rename the current branch)"),
        }
    } else if names.is_empty() {
        BranchOp::List {
            verbose: args.verbose,
        }
    } else {
        BranchOp::Create {
            name: names[0],
            start: names.get(1).copied(),
            force: args.force,
        }
    };

    let sig = current_signature(repo.git_dir())?;
    dispatch_branch(&repo, op, &sig)
}

fn cmd_tag(cli: &Cli, args: &TagArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let names: Vec<&str> = args.names.iter().map(String::as_str).collect();

    let op = if args.delete {
        if names.is_empty() {
            bail!("tag -d requires at least one tag name");
        }
        TagOp::Delete { names }
    } else if args.list || names.is_empty() {
        TagOp::List {
            show_annotation: args.show_annotation,
        }
    } else if args.annotate || args.message.is_some() {
        let message = args
            .message
            .as_deref()
            .context("annotated tag requires a message (-m)")?;
        TagOp::CreateAnnotated {
            name: names[0],
            target: names.get(1).copied(),
            message,
            force: args.force,
        }
    } else {
        TagOp::CreateLightweight {
            name: names[0],
            target: names.get(1).copied(),
            force: args.force,
        }
    };

    let sig = current_signature(repo.git_dir())?;
    dispatch_tag(&repo, op, Some(&sig))
}

fn cmd_switch_cli(cli: &Cli, args: &SwitchArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let result = cmd_switch(&repo, &args.target, args.create)?;
    report_switch(&result);
    Ok(())
}

fn cmd_checkout(cli: &Cli, args: &CheckoutArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let result = cmd_switch(&repo, &args.target, args.create)?;
    report_switch(&result);
    Ok(())
}

/// Emit a `git switch`-style confirmation line for a completed switch.
fn report_switch(result: &rit::SwitchResult) {
    match &result.head {
        SwitchHead::Symbolic(refname) => {
            let short = refname.strip_prefix("refs/heads/").unwrap_or(refname);
            println!("Switched to branch '{short}'");
        }
        SwitchHead::Detached(oid) => {
            println!("HEAD is now at {}", &oid.to_hex()[..7]);
        }
    }
}

fn cmd_diff_cli(cli: &Cli, args: &DiffArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let revs: Vec<&str> = args.revs.iter().map(String::as_str).collect();
    let mut out = std::io::stdout().lock();
    cmd_diff(&repo, args.cached, &revs, &mut out)?;
    Ok(())
}

fn cmd_show_cli(cli: &Cli, args: &ShowArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let mut out = std::io::stdout().lock();
    cmd_show(&repo, args.object.as_deref(), &mut out)?;
    Ok(())
}

fn git_identity(git_dir: &std::path::Path) -> Result<(String, String)> {
    let env_name = std::env::var("GIT_AUTHOR_NAME").ok();
    let env_email = std::env::var("GIT_AUTHOR_EMAIL").ok();
    if let (Some(name), Some(email)) = (env_name, env_email) {
        return Ok((name, email));
    }
    let config = std::fs::read_to_string(git_dir.join("config")).unwrap_or_default();
    let mut in_user = false;
    let mut name = None;
    let mut email = None;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_user = trimmed.eq_ignore_ascii_case("[user]");
            continue;
        }
        if !in_user {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            match key.trim().to_ascii_lowercase().as_str() {
                "name" => name = Some(value.trim().to_string()),
                "email" => email = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    match (name, email) {
        (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => Ok((name, email)),
        _ => bail!("configure user.name and user.email or GIT_AUTHOR_NAME/GIT_AUTHOR_EMAIL"),
    }
}

fn cmd_add(cli: &Cli, args: &AddArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    repo.add(&args.paths)?;
    Ok(())
}

fn cmd_status(cli: &Cli) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    for entry in repo.tracked_status()? {
        if entry.state == WorktreeState::Unmodified {
            continue;
        }
        let code = match entry.state {
            WorktreeState::Modified => " M",
            WorktreeState::Deleted => " D",
            WorktreeState::TypeChanged => " T",
            WorktreeState::Unmodified => continue,
        };
        println!("{code} {}", String::from_utf8_lossy(&entry.path));
    }
    Ok(())
}

fn cmd_init(args: &InitArgs) -> Result<()> {
    // Create the target directory itself if it does not exist yet (git init
    // <dir> creates the directory). This does not create missing parents beyond
    // what the user named, mirroring `git init`.
    if !args.directory.exists() {
        std::fs::create_dir_all(&args.directory)
            .with_context(|| format!("creating {}", args.directory.display()))?;
    }
    let existed = if args.bare {
        args.directory.join("HEAD").exists()
    } else {
        args.directory.join(".git").join("HEAD").exists()
    };
    let repo = Repository::init(&args.directory, args.bare, args.branch.as_deref())
        .with_context(|| format!("initializing repository at {}", args.directory.display()))?;
    let action = if existed {
        "Reinitialized existing"
    } else {
        "Initialized empty"
    };
    let kind = if args.bare { "bare " } else { "" };
    let git_dir = std::fs::canonicalize(repo.git_dir()).unwrap_or_else(|_| repo.git_dir().into());
    println!("{action} {kind}Git repository in {}/", git_dir.display());
    Ok(())
}

fn cmd_show_ref(cli: &Cli) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let refs = repo.refs().list()?;
    if refs.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    for (name, oid) in refs {
        writeln!(out, "{oid} {name}")?;
    }
    Ok(())
}

fn cmd_hash_object(cli: &Cli, args: &HashObjectArgs) -> Result<()> {
    let obj_type = ObjectType::parse(&args.object_type)
        .with_context(|| format!("invalid object type {:?}", args.object_type))?;

    let data = if args.stdin {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        buf
    } else {
        let file = args
            .file
            .as_ref()
            .context("provide a file path or use --stdin")?;
        std::fs::read(file).with_context(|| format!("reading {}", file.display()))?
    };

    // Writing requires a real repo; hashing-only does not.
    let oid = if args.write {
        let repo = Repository::discover(&cli.repo)
            .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
        repo.hash_object(obj_type, &data, true)?
    } else {
        rit::hash_content(obj_type, &data).0
    };
    println!("{oid}");
    Ok(())
}

fn cmd_cat_file(cli: &Cli, args: &CatFileArgs) -> Result<()> {
    if !args.show_type && !args.pretty {
        bail!("specify -t (type) or -p (pretty-print)");
    }
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let oid = repo.resolve(&args.object)?;

    if args.show_type {
        println!("{}", repo.object_type(&oid)?);
        return Ok(());
    }

    // pretty-print
    match repo.read_object(&oid)? {
        Object::Blob(b) => {
            std::io::stdout().write_all(&b.data)?;
        }
        Object::Tree(t) => {
            let mut out = std::io::stdout().lock();
            for e in &t.entries {
                writeln!(
                    out,
                    "{} {} {}\t{}",
                    e.display_mode(),
                    e.kind(),
                    e.oid,
                    e.name_str()
                )?;
            }
        }
        Object::Commit(c) => {
            std::io::stdout().write_all(&c.serialize())?;
        }
        Object::Tag(t) => {
            std::io::stdout().write_all(&t.serialize())?;
        }
    }
    Ok(())
}

fn cmd_ls_tree(cli: &Cli, args: &LsTreeArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let oid = repo.resolve(&args.rev)?;

    // Accept either a tree oid or a commit (list its tree).
    let tree = match repo.read_object(&oid)? {
        Object::Tree(t) => t,
        Object::Commit(c) => repo.tree(&c.tree)?,
        Object::Tag(_) => {
            let commit = repo.peel_to_commit(oid)?;
            repo.tree(&commit.tree)?
        }
        Object::Blob(_) => bail!("{oid} is a blob, not a tree"),
    };

    let mut out = std::io::stdout().lock();
    for e in &tree.entries {
        writeln!(
            out,
            "{} {} {}\t{}",
            e.display_mode(),
            e.kind(),
            e.oid,
            e.name_str()
        )?;
    }
    Ok(())
}

fn cmd_log(cli: &Cli, args: &LogArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let start = repo.resolve(&args.rev)?;
    let history = repo.log(start)?;

    let mut out = std::io::stdout().lock();
    for (oid, commit) in history.iter().take(args.max.unwrap_or(usize::MAX)) {
        writeln!(out, "commit {oid}")?;
        writeln!(
            out,
            "Author: {} <{}>",
            commit.author.name, commit.author.email
        )?;
        writeln!(out, "Date:   {} {}", commit.author.time, commit.author.tz)?;
        writeln!(out)?;
        for line in commit.message_str().lines() {
            writeln!(out, "    {line}")?;
        }
        writeln!(out)?;
    }
    Ok(())
}


// --- remotes: clone / remote / fetch / pull / push ---------------------------

#[derive(Args)]
struct CloneArgs {
    /// Remote URL (`https://host/org/repo.git`) or local repository path.
    url: String,
    /// Target directory (defaults to the repository name).
    directory: Option<PathBuf>,
    /// Create a bare repository (branches land in `refs/heads/*`).
    #[arg(long)]
    bare: bool,
}

#[derive(Args)]
struct RemoteArgs {
    #[command(subcommand)]
    action: Option<RemoteAction>,
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Add a remote.
    Add { name: String, url: String },
    /// Remove a remote.
    Remove { name: String },
    /// List remotes with their URLs.
    List,
}

#[derive(Args)]
struct FetchArgs {
    /// Remote to fetch from.
    #[arg(default_value = "origin")]
    remote: String,
}

#[derive(Args)]
struct PullArgs {
    /// Remote to pull from.
    #[arg(default_value = "origin")]
    remote: String,
}

#[derive(Args)]
struct PushArgs {
    /// Remote to push to.
    #[arg(default_value = "origin")]
    remote: String,
    /// `<src>[:<dst>]` (default: the current branch). `:<dst>` deletes `<dst>`.
    refspec: Option<String>,
    /// Allow a non-fast-forward update.
    #[arg(short, long)]
    force: bool,
}

/// Derive the default clone directory from a URL (`.../repo.git` -> `repo`).
fn clone_dir_name(url: &str) -> PathBuf {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("repo");
    PathBuf::from(last.strip_suffix(".git").unwrap_or(last))
}

fn cmd_clone(args: &CloneArgs) -> Result<()> {
    let directory = args.directory.clone().unwrap_or_else(|| clone_dir_name(&args.url));
    println!("Cloning into '{}'...", directory.display());
    let (_repo, report) = rit::remote::clone(&args.url, &directory, args.bare)?;
    if report.updated.is_empty() {
        println!("warning: You appear to have cloned an empty repository.");
    } else {
        println!(
            "Received {} object(s); {} ref(s) created.",
            report.objects,
            report.updated.len()
        );
    }
    Ok(())
}

fn cmd_remote(cli: &Cli, args: &RemoteArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    match &args.action {
        Some(RemoteAction::Add { name, url }) => {
            let remote = rit::remote::add_remote(&repo, name, url)?;
            println!("added remote {} -> {}", remote.name, remote.url);
        }
        Some(RemoteAction::Remove { name }) => {
            rit::remote::remove_remote(&repo, name)?;
            println!("removed remote {name}");
        }
        Some(RemoteAction::List) | None => {
            for remote in rit::remote::list_remotes(&repo)? {
                println!("{}\t{}", remote.name, remote.url);
            }
        }
    }
    Ok(())
}

fn print_fetch(remote: &str, report: &rit::remote::FetchReport) {
    if report.updated.is_empty() {
        println!("Already up to date with {remote}.");
        return;
    }
    for r in &report.updated {
        let old = r.old.map(|o| o.to_string()[..8].to_string()).unwrap_or_else(|| "(new)".into());
        println!(" {old}..{} {}", &r.new.to_string()[..8], r.local);
    }
    println!("Received {} object(s).", report.objects);
}

fn cmd_fetch(cli: &Cli, args: &FetchArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let report = rit::remote::fetch(&repo, &args.remote)?;
    print_fetch(&args.remote, &report);
    Ok(())
}

/// The current branch name (`refs/heads/<name>` -> `<name>`), if HEAD is attached.
fn current_branch(repo: &Repository) -> Result<String> {
    match repo.refs().read_head()? {
        rit::RefTarget::Symbolic(r) => r
            .strip_prefix("refs/heads/")
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("HEAD does not point at a branch")),
        rit::RefTarget::Direct(_) => bail!("HEAD is detached; switch to a branch first"),
    }
}

fn cmd_pull(cli: &Cli, args: &PullArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let branch = current_branch(&repo)?;
    let report = rit::remote::fetch(&repo, &args.remote)?;
    print_fetch(&args.remote, &report);
    let upstream = format!("refs/remotes/{}/{branch}", args.remote);
    let theirs = repo
        .refs()
        .resolve(&upstream)
        .with_context(|| format!("{} has no branch '{branch}'", args.remote))?;
    let local_ref = format!("refs/heads/{branch}");
    let ours = repo.refs().resolve(&local_ref).ok();
    if ours == Some(theirs) {
        println!("Already up to date.");
        return Ok(());
    }
    if let Some(ours) = ours {
        let ff = rit::is_ancestor(repo.odb(), ours, theirs, rit::ConnectivityLimits::default())?;
        if !ff {
            bail!(
                "branches have diverged; run `rit merge {}/{branch}` to combine them",
                args.remote
            );
        }
        let dirty = repo.tracked_status()?;
        if !dirty.is_empty() {
            bail!("you have local changes; commit or stash them before pulling");
        }
    }
    let tree = repo.commit(&theirs)?.tree;
    repo.checkout_tree(tree)?;
    repo.refs().compare_and_swap(&local_ref, ours, theirs)?;
    println!("Fast-forward {branch} to {}", &theirs.to_string()[..8]);
    Ok(())
}

fn cmd_push(cli: &Cli, args: &PushArgs) -> Result<()> {
    let repo = Repository::discover(&cli.repo)
        .with_context(|| format!("opening repo at {}", cli.repo.display()))?;
    let refspec = match &args.refspec {
        Some(r) => r.clone(),
        None => current_branch(&repo)?,
    };
    let report = rit::remote::push(&repo, &args.remote, &refspec, args.force)?;
    if report.up_to_date {
        println!("Everything up-to-date");
    } else {
        match (report.old, report.new) {
            (_, None) => println!(" - [deleted] {}", report.remote_ref),
            (None, Some(new)) => println!(" * [new] {} -> {}", &new.to_string()[..8], report.remote_ref),
            (Some(old), Some(new)) => println!(
                "   {}..{} -> {} ({} object(s))",
                &old.to_string()[..8],
                &new.to_string()[..8],
                report.remote_ref,
                report.objects
            ),
        }
    }
    Ok(())
}

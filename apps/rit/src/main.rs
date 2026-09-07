//! `rit` — a compatibility-oriented CLI and receive-pack process server.
//!
//! Runnable proof of the library: `hash-object`, `cat-file`, `ls-tree`, and
//! `log`, operating on real `.git` repositories.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use rit::object::Object;
use rit::{
    advertise_receive_pack, execute_receive_pack, parse_receive_pack_request, ObjectType,
    PacketLineLimits, ReceiveLimits, ReceivePackCommand, ReceivePackPolicy, Repository, Signature,
    WorktreeState, RECEIVE_PACK_CAPABILITIES,
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
    /// Restore tracked paths from the current index.
    Restore(RestoreArgs),
    /// Serve one stateful Git receive-pack session over stdin/stdout.
    ReceivePack(ReceivePackArgs),
    /// Serve Git upload-pack through the installed open-source Git backend.
    UploadPack(UploadPackArgs),
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
        Command::Restore(args) => cmd_restore(&cli, args),
        Command::ReceivePack(args) => cmd_receive_pack(args),
        Command::UploadPack(args) => cmd_upload_pack(args),
    }
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
            input.read_exact(&mut payload).context("reading pkt-line payload")?;
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
        let parsed = parse_receive_pack_request(&request, RECEIVE_PACK_CAPABILITIES, packet_limits)?;
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
    let needs_pack = parsed.commands.len() == 1
        && !matches!(parsed.commands[0], ReceivePackCommand::Delete { .. });
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
    let (name, email) = git_identity(repo.git_dir())?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs();
    let raw = format!("{name} <{email}> {seconds} +0000");
    let signature = Signature::parse(&raw, "commit")?;
    let oid = repo.commit_index(&args.message, signature.clone(), signature)?;
    println!("{oid}");
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
    println!(
        "{action} {kind}Git repository in {}/",
        git_dir.display()
    );
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

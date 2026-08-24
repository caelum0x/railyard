//! `rgf` — a small, compatibility-oriented CLI over the rithub core.
//!
//! Runnable proof of the library: `hash-object`, `cat-file`, `ls-tree`, and
//! `log`, operating on real `.git` repositories.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use rit::object::Object;
use rit::{ObjectType, Repository};

#[derive(Parser)]
#[command(
    name = "rgf",
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
    /// Compute an object id (and optionally write the object). Like `git hash-object`.
    HashObject(HashObjectArgs),
    /// Print an object's type or pretty content. Like `git cat-file`.
    CatFile(CatFileArgs),
    /// List the entries of a tree. Like `git ls-tree`.
    LsTree(LsTreeArgs),
    /// Show commit history from a revision (default HEAD). Like `git log`.
    Log(LogArgs),
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

fn main() {
    if let Err(err) = run() {
        eprintln!("rgf: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::HashObject(args) => cmd_hash_object(&cli, args),
        Command::CatFile(args) => cmd_cat_file(&cli, args),
        Command::LsTree(args) => cmd_ls_tree(&cli, args),
        Command::Log(args) => cmd_log(&cli, args),
    }
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

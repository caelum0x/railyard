//! Branch command handlers — the porcelain behind `rit branch`.
//!
//! Each `cmd_branch_*` function accepts a [`Repository`] reference plus the
//! parameters that the CLI argument parser supplies.  They return
//! `anyhow::Result<()>` so the integrator can call them directly from a
//! `main.rs` dispatch match arm without touching library internals.
//!
//! # CLI wiring (integrator notes)
//!
//! Add these clap variants to the `Command` enum in `main.rs`:
//!
//! ```text
//! Branch(BranchArgs)          → run(&cli, &args)
//! ```
//!
//! where `BranchArgs` contains:
//!
//! ```text
//! name:    Option<String>   // absent → list
//! start:   Option<String>   // defaults to HEAD
//! delete:  bool             // -d
//! force:   bool             // -D (implies delete) or -f (for create)
//! rename:  Option<String>   // -m <old>  (new = name)
//! verbose: bool             // -v
//! ```
//!
//! Then dispatch with:
//!
//! ```text
//! Command::Branch(args) => rit::branch::cmd_branch(&cli, args),
//! ```
//!
//! The convenience dispatcher [`cmd_branch`] decodes flags into the correct
//! sub-operation.  Alternatively call the individual `cmd_branch_*` functions
//! directly from more granular subcommands.

use anyhow::{bail, Context};
use std::io::Write;

use crate::refs::validate_refname;
use crate::{Repository, Signature};

// ── public handler functions ─────────────────────────────────────────────────

/// `rit branch` — list local branches, marking the current one with `*`.
///
/// With `verbose = true` each line shows the abbreviated tip OID and the first
/// line of its commit message (matching `git branch -v`).
pub fn cmd_branch_list(repo: &Repository, verbose: bool) -> anyhow::Result<()> {
    let branches = repo.branch_list().context("listing branches")?;
    if branches.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    for (name, oid, is_current) in &branches {
        let marker = if *is_current { '*' } else { ' ' };
        if verbose {
            let commit_msg = repo
                .peel_to_commit(*oid)
                .ok()
                .map(|c| {
                    c.message_str()
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
                .unwrap_or_default();
            let short = &oid.to_hex()[..7];
            writeln!(out, "{marker} {name:<20} {short} {commit_msg}")?;
        } else {
            writeln!(out, "{marker} {name}")?;
        }
    }
    Ok(())
}

/// `rit branch <name> [<start-point>]` — create a new local branch.
///
/// `start_point` is a revision string (`HEAD`, a branch name, a tag, or an
/// OID).  When `None` the current `HEAD` is used.
///
/// `force = true` moves an existing branch to `start_point` (like `git branch
/// -f`).
///
/// `sig` provides the identity written into the reflog entry.
pub fn cmd_branch_create(
    repo: &Repository,
    name: &str,
    start_point: Option<&str>,
    force: bool,
    sig: &Signature,
) -> anyhow::Result<()> {
    // Validate the short name now for a cleaner error message.
    let refname = format!("refs/heads/{name}");
    validate_refname(&refname).with_context(|| format!("invalid branch name {name:?}"))?;

    let start_rev = start_point.unwrap_or("HEAD");
    let start_oid = repo
        .resolve(start_rev)
        .with_context(|| format!("cannot resolve {start_rev:?}"))?;

    let source_desc = start_point.unwrap_or("HEAD");
    let message = format!("branch: Created from {source_desc}");

    repo.branch_create(
        name,
        start_oid,
        force,
        &sig.name,
        &sig.email,
        sig.time,
        &sig.tz,
        &message,
    )
    .with_context(|| format!("creating branch {name:?}"))?;

    Ok(())
}

/// `rit branch -d|-D <name>…` — delete one or more local branches.
///
/// `force = true` skips the "is the branch fully merged?" check
/// (equivalent to `git branch -D`).
///
/// Prints a confirmation line for each deleted branch.
pub fn cmd_branch_delete(
    repo: &Repository,
    names: &[&str],
    force: bool,
) -> anyhow::Result<()> {
    if names.is_empty() {
        bail!("branch --delete requires at least one branch name");
    }
    let mut out = std::io::stdout().lock();
    for name in names {
        let oid = repo
            .refs()
            .resolve(&format!("refs/heads/{name}"))
            .with_context(|| format!("branch {name:?} not found"))?;
        repo.branch_delete(name, force)
            .with_context(|| format!("deleting branch {name:?}"))?;
        writeln!(out, "Deleted branch {name} (was {}).", &oid.to_hex()[..7])?;
    }
    Ok(())
}

/// `rit branch -m <old-name> <new-name>` — rename a local branch.
///
/// `force = true` overwrites `new_name` if it already exists.
///
/// `sig` provides the identity written into the reflog entry.
pub fn cmd_branch_rename(
    repo: &Repository,
    old_name: &str,
    new_name: &str,
    force: bool,
    sig: &Signature,
) -> anyhow::Result<()> {
    repo.branch_rename(
        old_name,
        new_name,
        force,
        &sig.name,
        &sig.email,
        sig.time,
        &sig.tz,
    )
    .with_context(|| format!("renaming branch {old_name:?} → {new_name:?}"))?;
    println!("Renamed branch {old_name} to {new_name}.");
    Ok(())
}

// ── convenience dispatcher ────────────────────────────────────────────────────

/// Decoded branch operation — used by the convenience dispatcher.
#[derive(Debug)]
pub enum BranchOp<'a> {
    List { verbose: bool },
    Create { name: &'a str, start: Option<&'a str>, force: bool },
    Delete { names: Vec<&'a str>, force: bool },
    Rename { old: &'a str, new: &'a str, force: bool },
}

/// High-level branch dispatcher: given a decoded [`BranchOp`] and an actor
/// [`Signature`], run the appropriate sub-handler.
///
/// The integrator calls this from their `cmd_branch` main.rs function after
/// translating clap args into a `BranchOp`.
pub fn dispatch_branch(
    repo: &Repository,
    op: BranchOp<'_>,
    sig: &Signature,
) -> anyhow::Result<()> {
    match op {
        BranchOp::List { verbose } => cmd_branch_list(repo, verbose),
        BranchOp::Create { name, start, force } => {
            cmd_branch_create(repo, name, start, force, sig)
        }
        BranchOp::Delete { names, force } => cmd_branch_delete(repo, &names, force),
        BranchOp::Rename { old, new, force } => cmd_branch_rename(repo, old, new, force, sig),
    }
}

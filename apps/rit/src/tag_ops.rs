//! Tag command handlers — the porcelain behind `rit tag`.
//!
//! Each `cmd_tag_*` function accepts a [`Repository`] reference plus the
//! parameters that the CLI argument parser supplies.  They return
//! `anyhow::Result<()>` so the integrator can call them directly from a
//! `main.rs` dispatch match arm.
//!
//! # CLI wiring (integrator notes)
//!
//! Add a `Tag(TagArgs)` variant to the `Command` enum in `main.rs`:
//!
//! ```text
//! Command::Tag(args) => rit::tag_ops::cmd_tag(&cli, args),
//! ```
//!
//! where `TagArgs` contains:
//!
//! ```text
//! name:    Option<String>     // absent → list
//! target:  Option<String>     // revision the tag points at (default HEAD)
//! message: Option<String>     // -m: annotated tag when present
//! delete:  bool               // -d
//! force:   bool               // -f
//! list:    bool               // -l (explicit list; also default when name absent)
//! ```
//!
//! Then dispatch with the high-level convenience helper [`dispatch_tag`].

use anyhow::{bail, Context};
use std::io::Write;

use crate::{Repository, Signature};

// ── public handler functions ─────────────────────────────────────────────────

/// `rit tag` / `rit tag -l` — list all tags, one per line, alphabetically.
///
/// With `show_annotation = true` the first line of the annotation message is
/// printed after the tag name, separated by a space (matching `git tag -n1`).
pub fn cmd_tag_list(repo: &Repository, show_annotation: bool) -> anyhow::Result<()> {
    let tags = repo.tag_list().context("listing tags")?;
    if tags.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    for (name, oid) in &tags {
        if show_annotation {
            // Try to read the tag object for annotated tags; fall back to the
            // commit/blob/tree message when the ref is lightweight.
            let annotation = repo
                .read_object(oid)
                .ok()
                .and_then(|obj| match obj {
                    crate::object::Object::Tag(t) => {
                        t.message_str().lines().next().map(|l| l.trim().to_string())
                    }
                    crate::object::Object::Commit(c) => {
                        c.message_str().lines().next().map(|l| l.trim().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            if annotation.is_empty() {
                writeln!(out, "{name}")?;
            } else {
                writeln!(out, "{name:<20} {annotation}")?;
            }
        } else {
            writeln!(out, "{name}")?;
        }
    }
    Ok(())
}

/// `rit tag <name> [<target>]` — create a **lightweight** tag.
///
/// `target` is a revision string; defaults to `HEAD` when `None`.
/// `force = true` replaces an existing tag with the same name.
pub fn cmd_tag_create_lightweight(
    repo: &Repository,
    name: &str,
    target: Option<&str>,
    force: bool,
) -> anyhow::Result<()> {
    let rev = target.unwrap_or("HEAD");
    let target_oid = repo
        .resolve(rev)
        .with_context(|| format!("cannot resolve {rev:?}"))?;
    repo.tag_create_lightweight(name, target_oid, force)
        .with_context(|| format!("creating lightweight tag {name:?}"))?;
    Ok(())
}

/// `rit tag -a -m <msg> <name> [<target>]` — create an **annotated** tag.
///
/// Writes a tag object with `tagger`, `message`, and the resolved target type,
/// then creates the ref.  `force = true` replaces an existing tag.
///
/// Returns the OID of the written tag object.
pub fn cmd_tag_create_annotated(
    repo: &Repository,
    name: &str,
    target: Option<&str>,
    message: &str,
    tagger: &Signature,
    force: bool,
) -> anyhow::Result<crate::ObjectId> {
    if message.trim().is_empty() {
        bail!("annotated tag requires a non-empty message (-m)");
    }
    let rev = target.unwrap_or("HEAD");
    let target_oid = repo
        .resolve(rev)
        .with_context(|| format!("cannot resolve {rev:?}"))?;
    let target_type = repo
        .object_type(&target_oid)
        .with_context(|| format!("reading type of {}", target_oid.to_hex()))?;

    let tag_oid = repo
        .tag_create_annotated(
            name,
            target_oid,
            target_type,
            message.as_bytes(),
            tagger.clone(),
            force,
        )
        .with_context(|| format!("creating annotated tag {name:?}"))?;
    Ok(tag_oid)
}

/// `rit tag -d <name>…` — delete one or more tags.
///
/// Prints a confirmation line for each deleted tag.
pub fn cmd_tag_delete(repo: &Repository, names: &[&str]) -> anyhow::Result<()> {
    if names.is_empty() {
        bail!("tag --delete requires at least one tag name");
    }
    let mut out = std::io::stdout().lock();
    for name in names {
        let oid = repo
            .refs()
            .resolve(&format!("refs/tags/{name}"))
            .with_context(|| format!("tag {name:?} not found"))?;
        repo.tag_delete(name)
            .with_context(|| format!("deleting tag {name:?}"))?;
        writeln!(out, "Deleted tag {name} (was {}).", &oid.to_hex()[..7])?;
    }
    Ok(())
}

// ── convenience dispatcher ────────────────────────────────────────────────────

/// Decoded tag operation — used by the convenience dispatcher.
#[derive(Debug)]
pub enum TagOp<'a> {
    List {
        show_annotation: bool,
    },
    CreateLightweight {
        name: &'a str,
        target: Option<&'a str>,
        force: bool,
    },
    CreateAnnotated {
        name: &'a str,
        target: Option<&'a str>,
        message: &'a str,
        force: bool,
    },
    Delete {
        names: Vec<&'a str>,
    },
}

/// High-level tag dispatcher: given a decoded [`TagOp`] and an optional actor
/// [`Signature`] (required for annotated), run the appropriate sub-handler.
///
/// The integrator calls this from their `cmd_tag` main.rs function after
/// translating clap args into a `TagOp`.
pub fn dispatch_tag(
    repo: &Repository,
    op: TagOp<'_>,
    sig: Option<&Signature>,
) -> anyhow::Result<()> {
    match op {
        TagOp::List { show_annotation } => cmd_tag_list(repo, show_annotation),
        TagOp::CreateLightweight {
            name,
            target,
            force,
        } => cmd_tag_create_lightweight(repo, name, target, force),
        TagOp::CreateAnnotated {
            name,
            target,
            message,
            force,
        } => {
            let tagger =
                sig.ok_or_else(|| anyhow::anyhow!("annotated tag requires an actor signature"))?;
            cmd_tag_create_annotated(repo, name, target, message, tagger, force)?;
            Ok(())
        }
        TagOp::Delete { names } => cmd_tag_delete(repo, &names),
    }
}

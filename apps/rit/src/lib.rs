//! # rithub (core slice)
//!
//! A library-first Rust implementation of the **Git loose-object model**: read,
//! write, verify, and traverse standard Git repositories without corrupting
//! data. This crate is the narrow first promise from `PLAN.md` — the
//! collaboration *forge* (issues, pull requests, permissions, webhooks, wire
//! protocol) is deliberately **deferred** until this core passes interop,
//! crash-recovery, and fuzz testing.
//!
//! ## What is implemented
//!
//! - [`object`] — object types, canonical `"<type> <len>\0<payload>"` framing,
//!   SHA-1 object ids (byte-identical to real Git), and loose parsing.
//! - [`blob`], [`tree`], [`commit`], [`tag`] — read **and** write the four
//!   loose object kinds with byte-exact serialization.
//! - [`odb`] — the loose object database under `.git/objects/xx/...` with zlib
//!   compression and read-time SHA-1 verification.
//! - [`pack`] — **packed** object reads, adopted from **gitoxide** (`gix-odb` /
//!   `gix-pack`): objects living in `.git/objects/pack/*.pack` are resolved
//!   (including delta chains) through the same [`Object`]/[`Repository`] API, so
//!   callers get one interface over loose **or** packed objects. This is the
//!   OSS-fork strategy — the highest-cost from-scratch subsystem is reused, not
//!   rewritten, while `rit` keeps ownership of the loose path.
//! - [`refs`] — `HEAD`, loose refs, symbolic refs, and `packed-refs`.
//! - [`repo`] — the porcelain-ish [`Repository`] API: `hash-object`, `cat-file`,
//!   `rev-parse`, tree/commit traversal, and `log`.
//!
//! ## Compatibility
//!
//! Real-git interop is proven in `tests/interop.rs`: object ids computed here
//! are asserted identical to `git hash-object`, and repositories created by real
//! `git` are read back with this crate.
//!
//! ## Deferred (per PLAN)
//!
//! The index/worktree, the smart wire protocol, merge/rebase, SHA-256
//! repositories, SHA-1 collision detection (sha1dc), and the entire forge layer
//! are out of scope for this slice. Packfile **reads** are now covered by
//! adopting gitoxide (see [`pack`]); pack **writing** remains deferred.

pub mod blob;
pub mod commit;
pub mod error;
pub mod headers;
pub mod object;
pub mod odb;
pub mod oid;
pub mod pack;
pub mod refs;
pub mod repo;
pub mod signature;
pub mod tag;
pub mod tree;

pub use blob::Blob;
pub use commit::Commit;
pub use error::{ForgeError, Result};
pub use object::{hash_content, parse_loose, Object, ObjectType};
pub use odb::Odb;
pub use oid::ObjectId;
pub use pack::PackStore;
pub use refs::{RefTarget, Refs};
pub use repo::Repository;
pub use signature::Signature;
pub use tag::Tag;
pub use tree::{Tree, TreeEntry};

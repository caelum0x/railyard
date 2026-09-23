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
//! - [`refs`] — `HEAD`, loose refs, symbolic refs, `packed-refs`, and atomic CAS updates.
//! - [`wire`] — binary-safe pkt-line framing, receive-pack command/capability
//!   parsing, pack-boundary detection, and report-status encoding.
//! - [`fsck`] / [`receive`] — gitoxide-backed full/thin pack validation,
//!   bounded overlay-graph verification, and isolated decoded-object quarantine
//!   before immutable publication + one ref CAS.
//! - [`receive_pack`] — ref advertisement plus a complete one-ref receive-pack
//!   application boundary used by the CLI's stateful stdin/stdout server.
//! - [`index`] / [`worktree`] — verified index v2/v3 reads, atomic Git-compatible
//!   v2 mutation, recursive ignore-aware `add`, `write-tree`, and safe tracked status.
//! - [`repo`] — the porcelain-ish [`Repository`] API: `hash-object`, `cat-file`,
//!   `rev-parse`, tree/commit traversal, `log`, transactional `checkout-tree`,
//!   and explicit index-to-worktree `restore`. Materialization preflights all
//!   collisions before mutation and never follows a symlink outside the worktree.
//!
//! ## Compatibility
//!
//! Real-git interop is proven in `tests/interop.rs`: object ids computed here
//! are asserted identical to `git hash-object`, and repositories created by real
//! `git` are read back with this crate.
//!
//! ## Deferred (per PLAN)
//!
//! File/directory type-transition checkout, native Rust upload-pack/fetch,
//! Smart-HTTP/SSH listeners, multi-ref atomic receive, SHA-256
//! repositories, SHA-1 collision detection (sha1dc), and the entire forge layer
//! remain out of scope for this slice. A single-base three-way [`merge`]
//! (merge-base, tree merge, and content merge) is implemented and exported as a
//! library API but is intentionally not wired into the CLI; recursive
//! (multi-base/criss-cross) merge and rebase remain deferred — [`merge_commits`]
//! fails closed on a criss-cross history rather than guessing a base. Packfile reads and received full/thin pack
//! decoding adopt gitoxide (see [`pack`] and [`receive`]); clone/fetch currently
//! uses an explicit installed-Git upload-pack bridge. Rit retains limits,
//! quarantine, connectivity, publication, and ref-policy ownership.

pub mod blob;
pub mod branch;
pub mod checkout;
pub mod commit;
pub mod diff;
pub mod diff_render;
pub mod error;
pub mod fsck;
pub mod headers;
pub mod http;
pub mod index;
pub mod merge;
pub mod merge_cmd;
pub mod object;
pub mod odb;
pub mod oid;
pub mod pack;
pub mod receive;
pub mod receive_pack;
pub mod reflog;
pub mod refs;
pub mod repo;
pub mod sequencer;
pub mod server;
pub mod show;
pub mod signature;
pub mod stash;
pub mod tag;
pub mod tag_ops;
pub mod tree;
pub mod upload_pack;
pub mod wire;
pub mod worktree;

pub use blob::Blob;
pub use branch::{
    cmd_branch_create, cmd_branch_delete, cmd_branch_list, cmd_branch_rename, dispatch_branch,
    BranchOp,
};
pub use checkout::{cmd_switch, SwitchHead, SwitchResult};
pub use commit::Commit;
pub use diff::{diff_trees, ChangeKind, TreeChange};
pub use diff_render::cmd_diff;
pub use error::{ForgeError, Result};
pub use fsck::{
    check_connectivity, is_ancestor, ConnectivityLimits, ConnectivityReport, ObjectReader,
};
pub use http::smart_http::{
    info_refs, maybe_gunzip, receive_pack_rpc, router as smart_http_router,
    serve as smart_http_serve, serve_blocking as smart_http_serve_blocking, upload_pack_rpc,
    Service, SmartHttpConfig, SmartHttpState, SmartResponse, DEFAULT_MAX_BODY_BYTES,
};
pub use index::{Index, IndexEntry};
pub use merge::{merge_base, merge_bases, merge_commits, merge_trees, MergeOutcome};
pub use merge_cmd::{abort_merge, cmd_merge, continue_merge, MergeKind, MergeResult};
pub use object::{hash_content, parse_loose, Object, ObjectType};
pub use odb::Odb;
pub use oid::ObjectId;
pub use pack::PackStore;
pub use receive::{
    AtomicRefResult, AtomicRefUpdate, ReceiveAtomicOutcome, ReceiveLimits, ReceiveObject,
    ReceiveOutcome, ReceiveTransaction, RefUpdate,
};
pub use receive_pack::{
    advertise_receive_pack, execute_receive_pack, DeletedRef, ReceivePackExecution,
    ReceivePackPolicy, RECEIVE_PACK_CAPABILITIES,
};
pub use reflog::{append_reflog_file, log_path_for_ref, read_reflog, ReflogEntry, NULL_OID_HEX};
pub use refs::{validate_refname, AtomicRefError, RefChange, RefCommand, RefTarget, Refs};
pub use repo::Repository;
pub use sequencer::{
    cmd_cherry_pick, cmd_rebase, cmd_sequencer_abort, cmd_sequencer_continue, cmd_sequencer_skip,
    SequencerStatus,
};
pub use server::{dumb_info_refs, objects_info_packs, update_server_info};
pub use show::cmd_show;
pub use signature::Signature;
pub use stash::{
    cmd_stash_apply, cmd_stash_drop, cmd_stash_pop, cmd_stash_push, stash_list, StashApply,
};
pub use tag::Tag;
pub use tag_ops::{
    cmd_tag_create_annotated, cmd_tag_create_lightweight, cmd_tag_delete, cmd_tag_list,
    dispatch_tag, TagOp,
};
pub use tree::{Tree, TreeEntry};
pub use upload_pack::{
    advertise_upload_pack, generate_pack, generate_pack_with_options, parse_fetch_request,
    reachable_objects, upload_pack_v0, FetchRequest, PackOptions, UPLOAD_PACK_CAPABILITIES,
};
pub use wire::{
    decode_packet_lines, encode_packet_line, encode_receive_status, parse_receive_pack_request,
    PacketLine, PacketLineLimits, ReceivePackCommand, ReceivePackRequest, ReceiveStatus,
};
pub use worktree::{StatusEntry, WorktreeState};

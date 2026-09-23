//! # forge-search
//!
//! The forge **search + highlight** plane. Two independent capabilities, neither
//! of which owns any HTTP or database concern:
//!
//! 1. [`SearchIndex`] — a [Tantivy](https://docs.rs/tantivy)-backed full-text
//!    index over repositories, issues, pull requests, commit messages, and blob
//!    content. Documents are upserted incrementally as the forge is written to
//!    (see [`SearchIndex::upsert`]) and queried with visibility/permission
//!    scoping (see [`SearchIndex::search`] and [`SearchScope`]). The whole index
//!    can be rebuilt from a document stream with [`SearchIndex::reindex`].
//!
//! 2. [`highlight`] — pure-Rust ([syntect](https://docs.rs/syntect)) syntax
//!    highlighting that emits **class-based** HTML spans (never inline styles)
//!    for blob viewing and unified-diff rendering, plus the matching CSS
//!    ([`highlight::highlight_css`]).
//!
//! The crate is deliberately storage-agnostic: the caller (the forge server)
//! translates its own domain rows and git objects into [`IndexDocument`]s and
//! feeds blob/diff text into [`highlight`]. Keeping this boundary clean means
//! search can be swapped for a Postgres full-text backend without touching the
//! domain, and highlighting can be reused anywhere.

mod document;
pub mod highlight;
mod index;

pub use document::{DocKind, IndexDocument};
pub use index::{SearchError, SearchHit, SearchIndex, SearchScope};

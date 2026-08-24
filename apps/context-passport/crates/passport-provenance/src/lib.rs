//! Provenance: a durable record of which passport + locked version + provider
//! conditioned each generated asset, plus a C2PA-aligned manifest exporter.
//!
//! Design (from the provenance research): the rights in force are copied
//! (frozen) into each record so a later revocation cannot rewrite whether an
//! asset was licensed when it was made; PII/prompts are referenced by hash, never
//! inlined; and the record maps 1:1 onto a C2PA actions/ingredients manifest.

pub mod manifest;
pub mod record;
pub mod store;

pub use manifest::to_c2pa_manifest;
pub use record::GenerationRecord;
pub use store::{InMemoryProvenance, ProvenanceLog};

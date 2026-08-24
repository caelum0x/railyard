//! Character Lock Passport — a portable, lockable identity (character, brand, or
//! persona) that resolves into provider-specific conditioning across every AI
//! model provider on the Railyard rail.
//!
//! This is the Sx0 core loop from `VISION.md`:
//! create → edit (draft) → lock (immutable) → version (branch from a locked
//! version) → `resolve(passport_id, provider, modality)` into a conditioning
//! payload → record generation provenance.

pub mod domain;
pub mod error;
pub mod resolve;
pub mod server;
pub mod service;
pub mod store;

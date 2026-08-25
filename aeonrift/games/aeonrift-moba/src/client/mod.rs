//! The predicting/reconciling client.
//!
//! Built on the shared deterministic `sim`, split into:
//! - [`prediction`] — the [`Client`] struct + immediate local prediction,
//! - [`reconcile`]  — adopting authoritative snapshots + replaying pending inputs,
//! - [`input`]      — client-side input buffering + server-stamping helpers.

pub mod input;
pub mod prediction;
pub mod reconcile;

pub use prediction::Client;

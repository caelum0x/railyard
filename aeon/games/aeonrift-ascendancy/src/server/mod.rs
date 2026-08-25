//! # `server` — headless online-service seams.
//!
//! The authoritative-server side of Aeonrift: Ascendancy is deliberately thin here —
//! the deterministic [`crate::rules`] core IS the authoritative match. This module
//! provides the coordination seams a live service builds on. Today that is
//! [`matchmaking`]: a deterministic queue + lobby former. (Netcode / replication reuse
//! the engine's `net` crate and are out of scope for this crate.)

pub mod matchmaking;

pub use matchmaking::{Lobby, Matchmaker, QueuedPlayer};

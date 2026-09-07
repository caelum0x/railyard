//! `mint-server` — the AITP-402 settlement HTTP API (axum). Generic over the
//! `LedgerStore` so the SAME router/handlers/auth/error logic runs on
//! `InMemoryStore` (tests) and `PgStore` (production).
//!
//! - [`state`]   — [`AppState`] holding the four domain facades over one store.
//! - [`router`]  — [`build_router`] wiring every §4 route + auth.
//! - [`auth`]    — the constant-time `X-API-Key` middleware.
//! - [`error`]   — [`error::ApiError`] → HTTP status + JSON envelope (§7).
//! - [`dto`]     — request/response boundary types (µ$ `i64`, never floats).
//! - [`handlers`]— one thin module per resource group.
//! - [`shared`]  — `Arc`-sharing newtypes so the facades see one store.

pub mod attestation;
pub mod auth;
pub mod dto;
pub mod error;
pub mod handlers;
pub mod router;
pub mod shared;
pub mod state;
pub mod views;

pub use router::build_router;
pub use state::AppState;

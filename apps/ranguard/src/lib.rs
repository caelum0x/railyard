//! Vanguard-style Integrity Monitor — DEFENSIVE, USERSPACE, EDUCATIONAL.
//!
//! This library implements the honest, defensible subset of what an anti-cheat
//! does, entirely in safe userspace over the tool's OWN sandbox:
//!
//!   * `manifest`  — SHA-256 integrity manifest of the tool's own asset files,
//!                   signed with an HMAC, plus tamper detection on re-scan.
//!   * `process`   — attestation of a child process the tool ITSELF spawned.
//!   * `heartbeat` — challenge/response with a rolling HMAC (anti-replay).
//!   * `report`    — change/tamper reporting types.
//!
//! It deliberately does NOT do any of the things a real kernel anti-cheat does:
//! no kernel driver, no ring-0, no scanning of other processes' memory, no
//! anti-debugging of arbitrary processes, and no evasion. See the README's
//! SAFETY & ETHICS section.

#![forbid(unsafe_code)]

pub mod heartbeat;
pub mod hmac;
pub mod manifest;
pub mod process;
pub mod report;
pub mod sha256;

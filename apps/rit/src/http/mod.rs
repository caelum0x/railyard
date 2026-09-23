//! HTTP transport adapters for `rit`.
//!
//! This module owns the network-facing surface that lets a **stock `git`
//! client** clone, fetch, and push over HTTP against a repository served by
//! `rit`. It is deliberately thin: every byte of protocol semantics
//! (pkt-line framing, ref advertisement, negotiation, packfile generation,
//! quarantine, ref CAS) already lives in the synchronous core
//! (`upload_pack`, `receive_pack`, `wire`, `receive`). The code here only:
//!
//! 1. maps the Git **smart-HTTP** URL/verb grammar onto those core calls, and
//! 2. runs them on a blocking thread pool behind an async listener.
//!
//! No object, ref, or pack logic is reimplemented here — the adapter calls the
//! other modules' public functions and never reaches into their internals.
//!
//! See [`smart_http`] for the handlers, the router, and the listener.

pub mod smart_http;

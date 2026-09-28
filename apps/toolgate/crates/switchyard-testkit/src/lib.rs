//! # switchyard-testkit
//!
//! Fakes behind the same seams production uses, so tests exercise the real
//! pipeline: [`FakeHttp`] (upstream providers), [`FakeMint`] (holds, captures,
//! releases, balances, checkouts with AITP receipts), [`FakeRago`] (events,
//! with injectable failures), and [`fixtures`] (one provider-shaped response
//! per catalog tool).

pub mod fixtures;
mod http;
mod mint;
mod rago;

pub use http::{FakeHttp, FakeReply};
pub use mint::FakeMint;
pub use rago::FakeRago;

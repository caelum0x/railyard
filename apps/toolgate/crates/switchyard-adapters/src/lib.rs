//! # switchyard-adapters
//!
//! - [`declarative::DeclarativeAdapter`]: most public APIs need no Rust; the
//!   tool manifest's `request` block is rendered and its `output` mapping
//!   produces the capability's canonical output.
//! - [`native`]: typed adapters for our own products (Relay, Scribase, Rally,
//!   Menivor) and for public APIs whose shape needs code (arXiv Atom, RDAP
//!   bootstrap, SEC ticker lookup, package registries, readability).
//! - [`registry::build`]: one adapter per tool id from a loaded catalog.

pub mod declarative;
pub mod native;
pub mod registry;
pub mod util;

pub use registry::{build, AdapterMap};

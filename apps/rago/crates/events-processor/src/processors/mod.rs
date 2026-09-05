//! Port of the Go `processors` package.

pub mod events_processor;
pub mod main_processor;

pub use main_processor::{start_processing_events, Config};

//! In-memory stores implementing the `PassportStore` and `ProvenanceLog` ports.
//! Used by tests and embedded demos; the production path is Postgres.

mod memory;

pub use memory::MemoryStore;

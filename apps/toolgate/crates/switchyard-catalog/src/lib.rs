//! # switchyard-catalog
//!
//! Loads the catalog directory (`capabilities/`, `providers/`, `tools/`,
//! `bundles/`), checks every manifest (references resolve, schemas compile,
//! examples validate), and serves describe/search/validate.

pub mod loader;
pub mod mapping;
pub mod schema;
pub mod search;
pub mod template;

pub use loader::{BundleManifest, Catalog, CatalogError};
pub use search::{SearchFilters, SearchHit};

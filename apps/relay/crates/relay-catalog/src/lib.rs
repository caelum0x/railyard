//! Relay-owned model discovery and pricing service.

mod error;
mod model;
mod service;
mod source;

pub use error::{CatalogError, Result};
pub use model::{
    CatalogModel, CatalogSnapshot, ModelCapabilities, ModelPrice, PriceQuote, PriceQuoteRequest,
    PricingSource,
};
pub use service::CatalogService;
pub use source::{CatalogSource, JsonCatalogSource, StaticCatalogSource};

pub const CATALOG_SCHEMA_VERSION: u16 = 1;

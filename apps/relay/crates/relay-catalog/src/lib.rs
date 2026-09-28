//! Relay-owned model discovery and pricing service.

mod error;
pub mod litellm;
mod model;
pub mod openrouter;
mod service;
mod source;

pub use error::{CatalogError, Result};
pub use model::{
    CatalogModel, CatalogSnapshot, ModelCapabilities, ModelPrice, PriceQuote, PriceQuoteRequest,
    PricingSource,
};
pub use openrouter::{
    FeedOptions, HttpOpenRouterCatalogSource, MergeReport, OPENROUTER_MODELS_URL,
    merge_openrouter_feed,
};
pub use service::{CatalogService, bundled_snapshot};
pub use source::{CatalogSource, JsonCatalogSource, StaticCatalogSource};

pub const CATALOG_SCHEMA_VERSION: u16 = 1;

/// The checked-in list-price catalog (`data/catalog.json`): the same rows as
/// [`relay_billing::BUILTIN_PRICES`] with capabilities and context windows.
pub const BUNDLED_CATALOG_JSON: &str = include_str!("../data/catalog.json");

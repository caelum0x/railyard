use async_trait::async_trait;

use crate::{CatalogError, CatalogSnapshot, Result};

#[async_trait]
pub trait CatalogSource: Send + Sync {
    async fn load(&self) -> Result<CatalogSnapshot>;
}

#[derive(Debug, Clone)]
pub struct StaticCatalogSource {
    snapshot: CatalogSnapshot,
}

impl StaticCatalogSource {
    pub fn new(snapshot: CatalogSnapshot) -> Self {
        Self { snapshot }
    }
}

#[async_trait]
impl CatalogSource for StaticCatalogSource {
    async fn load(&self) -> Result<CatalogSnapshot> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Debug, Clone)]
pub struct JsonCatalogSource {
    json: String,
}

impl JsonCatalogSource {
    pub fn new(json: impl Into<String>) -> Self {
        Self { json: json.into() }
    }
}

#[async_trait]
impl CatalogSource for JsonCatalogSource {
    async fn load(&self) -> Result<CatalogSnapshot> {
        serde_json::from_str(&self.json)
            .map_err(|error| CatalogError::InvalidJson(error.to_string()))
    }
}

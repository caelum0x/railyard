//! Request bodies specific to the HTTP surface.

use serde::Deserialize;

use passport_resolve::Modality;

#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    pub provider: String,
    pub modality: Modality,
    #[serde(default)]
    pub version: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    pub provider: String,
    pub modality: Modality,
    #[serde(default)]
    pub version: Option<i64>,
}

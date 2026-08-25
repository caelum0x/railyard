//! Loading champion specs from RON text or files.
//!
//! Loading always runs BOTH validation layers: RON parsing (structural — closed
//! vocabulary) and [`crate::validate`] (semantic). A spec that survives both is
//! guaranteed compilable into the sim, so downstream code never has to re-check.

use std::path::Path;

use crate::spec::ChampionSpec;
use crate::validate::{validate, SpecError};

/// Parse and fully validate a champion spec from RON text.
///
/// Returns [`SpecError::Parse`] if the RON is malformed OR names an effect/
/// targeting/field outside the closed vocabulary (serde reports "unknown variant"
/// / "unknown field"). Otherwise runs semantic [`validate`].
pub fn load_str(ron_text: &str) -> Result<ChampionSpec, SpecError> {
    let spec: ChampionSpec =
        ron::from_str(ron_text).map_err(|e| SpecError::Parse(e.to_string()))?;
    validate(&spec)?;
    Ok(spec)
}

/// Parse and fully validate a champion spec from a `.ron` file on disk.
pub fn load_file(path: impl AsRef<Path>) -> Result<ChampionSpec, SpecError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|e| SpecError::Parse(format!("could not read {}: {e}", path.display())))?;
    load_str(&text)
}

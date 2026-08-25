//! Map + content loading.
//!
//! The map is authored as RON (same discipline as the champion DSL: named structs,
//! deny-unknown-fields) and describes LAYOUT — base/nexus positions, lane
//! waypoints, turret/inhibitor slots, and jungle/objective spawn points. Combat
//! STATS for structures and neutrals resolve from sibling catalogs so a map is pure
//! geometry:
//! - [`StructureStats`] from `content/turrets/structures.ron` (or a per-map override),
//! - the neutral catalog from `content/objectives/neutrals.ron`,
//! - the wave profile from `content/minions/wave.ron`,
//! - items from `content/items/*.ron`.
//!
//! Paths are resolved from `CARGO_MANIFEST_DIR` so loading works regardless of the
//! process working directory (tests, binaries, `cargo test` from anywhere).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sim::Vec2;

/// A 2D point in RON (`P(x: 1.0, y: 2.0)`), converted to [`Vec2`] on load.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct P {
    pub x: f32,
    pub y: f32,
}

impl From<P> for Vec2 {
    fn from(p: P) -> Vec2 {
        Vec2::new(p.x, p.y)
    }
}

/// Combat stats shared by every non-champion unit (structures, minions, neutrals).
/// In authoring units: `attack_speed` is attacks/second, compiled to a tick period.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitStats {
    pub hp: i32,
    pub ad: i32,
    pub armor: i32,
    pub range: f32,
    pub attack_speed: f32,
}

/// A team's base: nexus position + minion/champion spawn point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseDef {
    pub team: u8,
    pub nexus: P,
    pub spawn: P,
}

/// A turret slot: which team owns it, where it stands, and its tier (1 = outer).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurretDef {
    pub team: u8,
    pub pos: P,
    #[serde(default = "one")]
    pub tier: u8,
}

fn one() -> u8 {
    1
}

/// An inhibitor slot (a structure whose fall exposes the nexus).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InhibDef {
    pub team: u8,
    pub pos: P,
}

/// A lane: an ordered team0 -> team1 waypoint path plus its turrets/inhibitors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneDef {
    pub name: String,
    pub waypoints: Vec<P>,
    pub turrets: Vec<TurretDef>,
    #[serde(default)]
    pub inhibitors: Vec<InhibDef>,
}

/// A jungle camp spawn point; stats/rewards resolve from the neutral catalog by id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampDef {
    pub id: String,
    pub pos: P,
}

/// An epic objective spawn point; stats/rewards resolve from the neutral catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveDef {
    pub id: String,
    pub pos: P,
}

/// Base stats for the three destructible structure kinds, plus the near-infinite
/// armor a nexus wears while still protected (see `content/turrets/structures.ron`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureStats {
    pub turret: UnitStats,
    pub inhibitor: UnitStats,
    pub nexus: UnitStats,
    pub nexus_shield_armor: i32,
}

/// A fully-parsed map: layout + optional per-map structure override.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapData {
    pub name: String,
    pub bounds: f32,
    pub bases: Vec<BaseDef>,
    pub lanes: Vec<LaneDef>,
    /// Per-map override of the global structure catalog (tiny sandbox maps use it).
    #[serde(default)]
    pub structures: Option<StructureStats>,
    #[serde(default)]
    pub jungle: Vec<CampDef>,
    #[serde(default)]
    pub objectives: Vec<ObjectiveDef>,
}

impl MapData {
    /// The base belonging to `team`, if any.
    pub fn base(&self, team: u8) -> Option<&BaseDef> {
        self.bases.iter().find(|b| b.team == team)
    }

    /// The nexus position for `team`.
    pub fn nexus_pos(&self, team: u8) -> Option<Vec2> {
        self.base(team).map(|b| b.nexus.into())
    }
}

/// The absolute path to the repo's `content/` directory, resolved at compile time
/// from this crate's manifest dir (`games/aeonrift-moba` -> `../../content`).
pub fn content_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
}

/// Errors surfaced while loading content RON.
#[derive(Debug)]
pub enum ContentError {
    Io(String),
    Parse(String),
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentError::Io(e) => write!(f, "content io error: {e}"),
            ContentError::Parse(e) => write!(f, "content parse error: {e}"),
        }
    }
}

impl std::error::Error for ContentError {}

/// Parse a RON value of type `T` from text, mapping parse failures to a
/// [`ContentError`] (unknown fields/variants are rejected — closed vocabulary).
pub fn parse_ron<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, ContentError> {
    ron::from_str(text).map_err(|e| ContentError::Parse(e.to_string()))
}

/// Read + parse a RON file of type `T`.
pub fn load_ron<T: for<'de> Deserialize<'de>>(path: impl AsRef<Path>) -> Result<T, ContentError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|e| ContentError::Io(format!("{}: {e}", path.display())))?;
    parse_ron(&text)
}

/// Load a map by file stem from `content/maps/<name>.ron`.
pub fn load_map(name: &str) -> Result<MapData, ContentError> {
    load_ron(content_dir().join("maps").join(format!("{name}.ron")))
}

/// Load the global structure catalog from `content/turrets/structures.ron`.
pub fn load_structures() -> Result<StructureStats, ContentError> {
    load_ron(content_dir().join("turrets").join("structures.ron"))
}

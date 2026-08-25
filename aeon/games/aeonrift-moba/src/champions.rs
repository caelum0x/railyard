//! Champion roster loading.
//!
//! Reuses the ability-DSL crate (its lib is named `champions`, reached here via the
//! extern path `::champions::` so it does not clash with this module) to load +
//! validate every `content/champions/*.ron` spec, then compiles a chosen spec into a
//! live [`sim::Champion`] via the DSL interpreter. The roster is the same data the
//! duel/roster tests already gate — here it becomes the pool of PLAYABLE units a
//! match drafts from.

use ::champions::{compile, load_file, ChampionSpec};
use sim::{Champion, EntityId, Vec2};

use crate::rules::map::{content_dir, ContentError};

/// One loaded, validated champion: its slug (file stem) + parsed spec.
#[derive(Clone, Debug, PartialEq)]
pub struct RosterEntry {
    pub slug: String,
    pub spec: ChampionSpec,
}

/// Load + validate the whole champion roster from `content/champions/*.ron`,
/// skipping the `roster_meta` sidecar and any non-`.ron` files. Sorted by slug for
/// a deterministic draft order.
pub fn load_roster() -> Result<Vec<RosterEntry>, ContentError> {
    let dir = content_dir().join("champions");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| ContentError::Io(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "ron").unwrap_or(false))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s != "roster_meta")
                .unwrap_or(false)
        })
        .collect();
    paths.sort();

    let mut roster = Vec::with_capacity(paths.len());
    for path in paths {
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let spec = load_file(&path)
            .map_err(|e| ContentError::Parse(format!("{}: {e:?}", path.display())))?;
        roster.push(RosterEntry { slug, spec });
    }
    Ok(roster)
}

/// Compile a spec into a live sim champion (base stats -> combat stats) at `pos`.
pub fn playable(spec: &ChampionSpec, id: EntityId, team: u8, pos: Vec2) -> Champion {
    compile(spec, id, team, pos)
}

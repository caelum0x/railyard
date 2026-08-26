//! Recording a match into per-beat board snapshots — the seeing seam.
//!
//! The headless replay gallery needs to *watch* a match without a window: it samples
//! the deterministic core every few ticks into a plain-data [`Snapshot`] (elixir,
//! base HP, and every live unit's lane / progress / HP / **card slug**), so a renderer
//! (HTML today, the Bevy client tomorrow) can draw the board from the real cards.
//!
//! The recorder replays the SAME one-tick pipeline as [`super::runner::step_tick`]
//! (bot decisions, then regen, deploy, movement, sim step, win check), inlined only so
//! it can capture each deployed unit's entity id => card slug. It is therefore a pure
//! function of `(config, decks, interval)` and produces a match whose final state hash
//! is identical to [`super::runner::run_bot_match`] — asserted in the tests.

use std::collections::HashMap;

use sim::EntityId;

use super::config::MatchConfig;
use super::lanes;
use super::match_state::{MatchState, Outcome};
use crate::ai::deploy_bot::bot_deploys;
use crate::cards::card::Card;
use crate::rules::deploy::try_deploy;
use crate::rules::elixir::regen_elixir;
use crate::rules::unit_ai::movement_inputs;

/// A deck entry that remembers its champion **slug** (so a deployed unit can be tied
/// back to its finished card face). Wraps the playable [`Card`] verbatim.
#[derive(Clone, Debug, PartialEq)]
pub struct SlugCard {
    pub slug: String,
    pub card: Card,
}

impl SlugCard {
    pub fn new(slug: impl Into<String>, card: Card) -> Self {
        SlugCard {
            slug: slug.into(),
            card,
        }
    }
}

/// One live unit as seen on the board at a sampled tick.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitView {
    pub team: u8,
    /// Lane column (0-based), snapped from the unit's x.
    pub lane: usize,
    /// The unit's card slug (its finished face) and display name.
    pub slug: String,
    pub name: String,
    /// Current / max HP and the 0..=1 fraction (for a health bar).
    pub hp: i32,
    pub hp_max: i32,
    pub hp_frac: f32,
    /// March progress from the unit's own base (0.0) to the enemy base (1.0).
    pub progress: f32,
}

/// The whole board at one sampled tick — pure data, engine-neutral, serialisable.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub tick: u64,
    pub elixir: [u32; 2],
    pub base_hp: [i32; 2],
    pub base_hp_max: i32,
    pub units: Vec<UnitView>,
    /// `Some` only on the terminal snapshot.
    pub outcome: Option<Outcome>,
}

/// A recorded match: its config, the ordered snapshots, and the final result.
#[derive(Clone, Debug)]
pub struct MatchReplay {
    pub config: MatchConfig,
    pub deck_slugs: [Vec<String>; 2],
    pub snapshots: Vec<Snapshot>,
    pub outcome: Outcome,
    pub final_tick: u64,
    pub final_hash: u64,
}

/// March progress (0 at own base, 1 at enemy base) for a unit at world-y `y`.
fn progress_for(cfg: &MatchConfig, team: u8, y: f32) -> f32 {
    let own = if team == 0 { -cfg.base_y } else { cfg.base_y };
    let enemy = if team == 0 { cfg.base_y } else { -cfg.base_y };
    let span = enemy - own;
    if span.abs() < f32::EPSILON {
        return 0.0;
    }
    ((y - own) / span).clamp(0.0, 1.0)
}

/// Build the board snapshot for the current match state, resolving each live unit's
/// slug from the `id => (slug, name)` map filled at deploy time.
fn snapshot(m: &MatchState, id_slug: &HashMap<EntityId, (String, String)>) -> Snapshot {
    let cfg = &m.config;
    let mut units = Vec::new();
    for champ in &m.world.champions {
        if m.is_base(champ.id) || !champ.alive {
            continue;
        }
        let (slug, name) = match id_slug.get(&champ.id) {
            Some((s, n)) => (s.clone(), n.clone()),
            None => continue, // never happens: every unit is deployed from a card
        };
        let hp_max = champ.health.max.max(1);
        let hp = champ.health.current.max(0);
        units.push(UnitView {
            team: champ.team,
            lane: lanes::nearest_lane(cfg, champ.pos.x),
            slug,
            name,
            hp,
            hp_max: champ.health.max,
            hp_frac: (hp as f32 / hp_max as f32).clamp(0.0, 1.0),
            progress: progress_for(cfg, champ.team, champ.pos.y),
        });
    }
    Snapshot {
        tick: m.world.tick,
        elixir: m.elixir,
        base_hp: [m.base_hp(0), m.base_hp(1)],
        base_hp_max: cfg.base_hp,
        units,
        outcome: m.outcome,
    }
}

/// Inline of [`super::win::evaluate`], kept here so the recorder's tick pipeline is
/// byte-identical to `step_tick` without depending on a private re-export.
fn evaluate(m: &MatchState) -> Option<Outcome> {
    match (m.base_alive(0), m.base_alive(1)) {
        (true, true) => None,
        (true, false) => Some(Outcome::Winner(0)),
        (false, true) => Some(Outcome::Winner(1)),
        (false, false) => Some(Outcome::Draw),
    }
}

/// Record a deterministic bot-vs-bot match, sampling a [`Snapshot`] every
/// `snapshot_every` ticks (plus the opening tick 0 and the terminal tick).
///
/// Same `(config, decks, interval)` => identical snapshots and an identical final hash
/// to [`super::runner::run_bot_match`]. `snapshot_every` is clamped to `>= 1`.
pub fn record_match(
    config: MatchConfig,
    deck0: Vec<SlugCard>,
    deck1: Vec<SlugCard>,
    interval: u64,
    snapshot_every: u64,
) -> MatchReplay {
    let every = snapshot_every.max(1);
    let deck_slugs = [
        deck0.iter().map(|c| c.slug.clone()).collect::<Vec<_>>(),
        deck1.iter().map(|c| c.slug.clone()).collect::<Vec<_>>(),
    ];
    let cards0: Vec<Card> = deck0.iter().map(|c| c.card.clone()).collect();
    let cards1: Vec<Card> = deck1.iter().map(|c| c.card.clone()).collect();
    // Deck index => (slug, display name), for tagging deployed units.
    let deck_meta: [Vec<(String, String)>; 2] = [
        deck0
            .iter()
            .map(|c| (c.slug.clone(), c.card.name().to_string()))
            .collect(),
        deck1
            .iter()
            .map(|c| (c.slug.clone(), c.card.name().to_string()))
            .collect(),
    ];

    let mut m = MatchState::new(config, cards0, cards1);
    let mut id_slug: HashMap<EntityId, (String, String)> = HashMap::new();
    let mut snapshots: Vec<Snapshot> = Vec::new();

    // Opening board (tick 0, nothing deployed yet).
    snapshots.push(snapshot(&m, &id_slug));

    while !m.is_over() && m.world.tick < config.max_ticks {
        // --- step_tick, inlined so we can capture deploy ids ---
        let deploys = bot_deploys(&m, interval);
        regen_elixir(&mut m);
        for d in &deploys {
            if let Ok(id) = try_deploy(&mut m, *d) {
                let (slug, name) = deck_meta[d.player as usize][d.card].clone();
                id_slug.insert(id, (slug, name));
            }
        }
        let inputs = movement_inputs(&m);
        sim::step(&mut m.world, &inputs);
        m.outcome = evaluate(&m);
        // --- end step_tick ---

        if m.is_over() || m.world.tick.is_multiple_of(every) {
            snapshots.push(snapshot(&m, &id_slug));
        }
    }
    if !m.is_over() {
        m.outcome = Some(Outcome::Draw);
        snapshots.push(snapshot(&m, &id_slug));
    }

    MatchReplay {
        config,
        deck_slugs,
        outcome: m.outcome.unwrap_or(Outcome::Draw),
        final_tick: m.world.tick,
        final_hash: crate::rules::match_state::match_hash(&m),
        snapshots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::card_pool::CardPool;
    use crate::rules::runner::run_bot_match;

    fn deck(pool: &CardPool, slugs: &[&str]) -> Vec<SlugCard> {
        slugs
            .iter()
            .map(|s| SlugCard::new(*s, pool.find_by_slug(s).expect("slug in roster").clone()))
            .collect()
    }

    #[test]
    fn recorded_match_matches_canonical_runner_hash() {
        let pool = CardPool::load_default().expect("pool");
        let cfg = MatchConfig::default();
        let d0 = deck(&pool, &["sentinel", "vanguard"]);
        let d1 = deck(&pool, &["pyromancer"]);

        let replay = record_match(cfg, d0.clone(), d1.clone(), 30, 300);
        // The canonical runner over the same cards must land on the same final hash:
        // the recorder is the same match, only observed.
        let canonical = run_bot_match(
            cfg,
            d0.iter().map(|c| c.card.clone()).collect(),
            d1.iter().map(|c| c.card.clone()).collect(),
            30,
        );
        assert_eq!(replay.final_hash, crate::match_hash(&canonical));
        assert_eq!(replay.final_tick, canonical.world.tick);
    }

    #[test]
    fn recording_is_deterministic() {
        let pool = CardPool::load_default().expect("pool");
        let cfg = MatchConfig::default();
        let d0 = deck(&pool, &["sentinel", "vanguard"]);
        let d1 = deck(&pool, &["pyromancer"]);
        let a = record_match(cfg, d0.clone(), d1.clone(), 30, 300);
        let b = record_match(cfg, d0, d1, 30, 300);
        assert_eq!(a.final_hash, b.final_hash);
        assert_eq!(a.snapshots, b.snapshots);
    }

    #[test]
    fn snapshots_open_empty_and_close_decided() {
        let pool = CardPool::load_default().expect("pool");
        let cfg = MatchConfig::default();
        let d0 = deck(&pool, &["sentinel", "vanguard"]);
        let d1 = deck(&pool, &["pyromancer"]);
        let replay = record_match(cfg, d0, d1, 30, 300);
        assert!(replay.snapshots.len() >= 2, "at least opening + terminal");
        // Opening board: tick 0, no units.
        let first = &replay.snapshots[0];
        assert_eq!(first.tick, 0);
        assert!(first.units.is_empty());
        // Terminal board carries the outcome.
        let last = replay.snapshots.last().unwrap();
        assert!(last.outcome.is_some());
        // Every unit view resolved a real slug + face.
        for s in &replay.snapshots {
            for u in &s.units {
                assert!(!u.slug.is_empty());
                assert!((0.0..=1.0).contains(&u.progress));
                assert!(crate::cards::face::face_exists(&u.slug));
            }
        }
    }
}

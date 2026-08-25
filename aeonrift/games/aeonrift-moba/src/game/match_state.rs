//! The match driver: the whole MOBA tick, built on `sim::step`.
//!
//! A [`Match`] owns the sim world plus the MOBA state the sim does not:
//! the loaded map, the entity registry, the economy, the wave scheduler, and the
//! phase. Each [`Match::step`] runs the full deterministic MOBA tick:
//! spawn waves -> gather AI/minion inputs -> `sim::step` -> pay out deaths + apply
//! neutral buffs -> expose nexuses -> check the win condition.
//!
//! Determinism: no wall-clock, no RNG beyond the sim's seeded stream, and every
//! iteration is over id-sorted vecs, so `Match::new(mode, seed)` + the same input
//! sequence yields a byte-identical trajectory and [`Match::fingerprint`].

use sim::{state_hash, EntityId, Input, Vec2, World};

use crate::champions;
use crate::modes::GameMode;
use crate::rules::map::{load_map, load_structures, ContentError, MapData, StructureStats};
use crate::rules::objectives::NeutralCatalog;
use crate::rules::waves::WaveProfile;
use crate::rules::{
    alloc_id, enemy_of, inhibitors, jungle, lanes, nexus, objectives, turrets, waves, Registry,
    Unit, UnitKind, BLUE, NEUTRAL, RED,
};

use super::economy::Economy;
use super::phases::Phase;

/// Gold awarded to the enemy team for killing a champion.
pub const CHAMPION_BOUNTY: i32 = 300;

/// A single running MOBA match.
pub struct Match {
    pub world: World,
    pub registry: Registry,
    pub map: MapData,
    pub structures: StructureStats,
    pub waves: WaveProfile,
    pub neutrals: NeutralCatalog,
    pub economy: Economy,
    pub phase: Phase,
    pub mode: GameMode,
    next_id: EntityId,
    /// Dead entity ids already paid out, kept sorted for O(log n) membership.
    awarded_dead: Vec<EntityId>,
    /// Champion ids driven by the laning bot.
    bot_ids: Vec<EntityId>,
}

impl Match {
    /// Build a match for `mode`, seeding the sim with `seed`. Loads the map + all
    /// content catalogs, drafts champions from the roster, and spawns structures +
    /// neutrals. Returns once the world is fully built and ready to tick.
    pub fn new(mode: GameMode, seed: u64) -> Result<Self, ContentError> {
        let map = load_map(mode.map_name)?;
        let structures = match map.structures {
            Some(s) => s,
            None => load_structures()?,
        };
        let waves = waves::load_waves()?;
        let neutrals = objectives::load_neutrals()?;
        let roster = champions::load_roster()?;
        if roster.is_empty() {
            return Err(ContentError::Parse("champion roster is empty".into()));
        }

        let mut world = World::from_champions(seed, Vec::new());
        let mut registry = Registry::new();
        let mut next_id: EntityId = 0;
        let mut bot_ids = Vec::new();

        // Draft + spawn champions for both teams (deterministic pick from the roster).
        for &team in &[BLUE, RED] {
            let base_spawn: Vec2 = map
                .base(team)
                .map(|b| b.spawn.into())
                .unwrap_or(Vec2::ZERO);
            let team_offset = if team == BLUE { 0 } else { mode.team_size };
            for i in 0..mode.team_size {
                let entry = &roster[(team_offset + i) % roster.len()];
                let id = alloc_id(&mut next_id);
                let pos = spread(base_spawn, i, map.bounds);
                let champ = champions::playable(&entry.spec, id, team, pos);
                world.champions.push(champ);
                registry.add(Unit::stationary(
                    id,
                    UnitKind::Champion,
                    team,
                    CHAMPION_BOUNTY,
                    None,
                ));
                if team == RED && mode.bots_on_red {
                    bot_ids.push(id);
                }
            }
        }

        // Structures, then neutrals (ids stay ascending: registry invariant holds).
        turrets::spawn(&mut world, &mut registry, &mut next_id, &map, &structures);
        inhibitors::spawn(&mut world, &mut registry, &mut next_id, &map, &structures);
        nexus::spawn(&mut world, &mut registry, &mut next_id, &map, &structures);
        jungle::spawn(&mut world, &mut registry, &mut next_id, &map, &neutrals);
        objectives::spawn(&mut world, &mut registry, &mut next_id, &map, &neutrals);

        Ok(Match {
            world,
            registry,
            map,
            structures,
            waves,
            neutrals,
            economy: Economy::new(mode.starting_gold),
            phase: Phase::InGame,
            mode,
            next_id,
            awarded_dead: Vec::new(),
            bot_ids,
        })
    }

    /// The champion ids controlled by the AI (RED bots in coop-vs-bots).
    pub fn bot_ids(&self) -> &[EntityId] {
        &self.bot_ids
    }

    /// Advance the whole match by one deterministic tick. `player_inputs` are the
    /// externally-supplied commands (from human clients / a test script). A no-op
    /// once the match has ended.
    pub fn step(&mut self, player_inputs: &[Input]) {
        if self.phase.is_over() {
            return;
        }
        let tick = self.world.tick;

        // 1. Spawn a wave if this tick is a wave tick.
        waves::maybe_spawn(
            &self.waves,
            tick,
            &mut self.world,
            &mut self.registry,
            &mut self.next_id,
            &self.map,
        );

        // 2. Assemble this tick's inputs: players, then minion AI, then bot AI.
        let mut inputs: Vec<Input> = player_inputs.to_vec();
        inputs.extend(lanes::steer_minions(&self.map, &mut self.registry, &self.world));
        for &bid in &self.bot_ids {
            if let Some(c) = self.world.champion(bid) {
                if c.alive {
                    let goal = self.map.nexus_pos(enemy_of(c.team)).unwrap_or(c.pos);
                    inputs.extend(crate::ai::decide(&self.world, &self.registry, bid, goal));
                }
            }
        }

        // 3. Advance the shared deterministic sim.
        sim::step(&mut self.world, &inputs);

        // 4. Pay out newly-dead units + apply neutral buffs.
        self.process_deaths();

        // 5. Expose any nexus whose protectors have all fallen.
        nexus::expose_ready(&mut self.world, &self.registry, &self.structures);

        // 6. Win check.
        if let Some(winner) = nexus::winner(&self.world, &self.registry) {
            self.phase = Phase::Ended { winner };
        }
    }

    /// The winning team, if the match has ended.
    pub fn winner(&self) -> Option<u8> {
        self.phase.winner()
    }

    /// A full deterministic fingerprint of the match state: the sim state hash folded
    /// with the economy and phase. Two matches with the same seed + inputs produce
    /// identical fingerprints every tick.
    pub fn fingerprint(&self) -> u64 {
        const K: u64 = 0x0100_0000_01b3; // FNV-ish mixing constant
        let mut h = state_hash(&self.world);
        h = h.wrapping_mul(K).wrapping_add(self.economy.gold(BLUE) as u64);
        h = h.wrapping_mul(K).wrapping_add(self.economy.gold(RED) as u64);
        h = h.wrapping_mul(K).wrapping_add(self.economy.xp(BLUE) as u64);
        h = h.wrapping_mul(K).wrapping_add(self.economy.xp(RED) as u64);
        h = h.wrapping_mul(K).wrapping_add(self.phase.code());
        h
    }

    /// Award gold + buffs for every unit that died this tick (idempotent per id).
    fn process_deaths(&mut self) {
        let newly: Vec<EntityId> = self
            .world
            .champions
            .iter()
            .filter(|c| !c.alive)
            .map(|c| c.id)
            .filter(|id| self.awarded_dead.binary_search(id).is_err())
            .collect();

        for id in newly {
            let (team, bounty, neutral_id) = match self.registry.get(id) {
                Some(u) => (u.team, u.bounty, u.neutral_id.clone()),
                None => continue,
            };

            let killer_team = if team == NEUTRAL {
                self.nearest_champion_team(id)
            } else {
                Some(enemy_of(team))
            };

            if let Some(kt) = killer_team {
                self.economy.award(kt, bounty, bounty / 2);
                if team == NEUTRAL {
                    if let Some(def) = self.neutrals.get(&neutral_id) {
                        if let Some(buff) = def.buff {
                            objectives::apply_team_buff(&mut self.world, &self.registry, kt, &buff);
                        }
                    }
                }
            }

            let pos = self.awarded_dead.binary_search(&id).unwrap_or_else(|e| e);
            self.awarded_dead.insert(pos, id);
        }
    }

    /// The player team of the living champion nearest to `neutral_id` (tie -> lower
    /// id). Used to attribute neutral-unit kills deterministically.
    fn nearest_champion_team(&self, neutral_id: EntityId) -> Option<u8> {
        let npos = self.world.champion(neutral_id)?.pos;
        let mut best: Option<(u8, f32, EntityId)> = None;
        for u in self.registry.iter() {
            if u.kind != UnitKind::Champion || (u.team != BLUE && u.team != RED) {
                continue;
            }
            let Some(c) = self.world.champion(u.id) else {
                continue;
            };
            if !c.alive {
                continue;
            }
            let d = npos.distance(c.pos);
            let better = match best {
                None => true,
                Some((_, bd, bid)) => d < bd || (d == bd && u.id < bid),
            };
            if better {
                best = Some((u.team, d, u.id));
            }
        }
        best.map(|(t, _, _)| t)
    }
}

/// Spread a team's champions around their spawn point so they do not stack exactly
/// (deterministic per-index offset, clamped to the play-plane).
fn spread(spawn: Vec2, i: usize, bounds: f32) -> Vec2 {
    let k = i as f32;
    Vec2::new(spawn.x + k * 1.5, spawn.y + k * 1.5).clamp_square(bounds)
}

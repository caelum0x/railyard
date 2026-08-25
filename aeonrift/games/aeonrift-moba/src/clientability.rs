//! Client-side ability state: cooldowns, charges, and a resource pool (mana).
//!
//! ## Why this is hand-rolled instead of `leafwing_abilities`
//! The plan was to use `leafwing_abilities` (`Cooldown` / `Charges` / `Pool` /
//! `AbilityState`) here. Its only published versions target **Bevy 0.18-rc.2 and
//! older** (0.12.0 → Bevy `^0.18.0-rc.2` + `leafwing-input-manager 0.20`; 0.11.0 →
//! Bevy 0.16). This game's client is **Bevy 0.19** (matching `leafwing-input-manager
//! 0.21`), so `leafwing_abilities` cannot resolve into the client dependency graph
//! without dragging in a second, incompatible Bevy. Rather than fork the crate, this
//! module provides a tiny, Bevy-0.19-native equivalent that mirrors its API surface
//! (`Cooldown::ready/trigger/tick`, `Charges`, `Pool::available/expend/replenish`)
//! so the ability-UX wiring in [`crate::clientinput`] is identical in spirit.
//!
//! ## What it is (and is NOT)
//! This is the CLIENT presentation / UX layer only. The authoritative sim already
//! gates ability cooldowns deterministically (`sim::systems::apply_inputs`); these
//! client cooldowns/pool only decide whether a local press is *offered* to the sim
//! as an intent (so the HUD can grey out an on-cooldown ability and refuse a cast
//! with no mana), never whether it actually resolves. The sim remains the source of
//! truth.

use bevy::prelude::*;

/// A single ability cooldown, measured in seconds. Immutable-friendly: the mutating
/// helpers return nothing but never alias, and pure queries take `&self`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cooldown {
    /// Full cooldown duration in seconds.
    max_secs: f32,
    /// Seconds elapsed since the last trigger (clamped to `max_secs`).
    elapsed_secs: f32,
}

impl Cooldown {
    /// A cooldown of `max_secs` seconds, starting fully ready.
    pub fn from_secs(max_secs: f32) -> Self {
        Cooldown {
            max_secs: max_secs.max(0.0),
            elapsed_secs: max_secs.max(0.0),
        }
    }

    /// True once at least `max_secs` have elapsed since the last trigger.
    pub fn ready(&self) -> bool {
        self.elapsed_secs >= self.max_secs
    }

    /// Advance the cooldown by `delta` seconds (call once per frame/tick).
    pub fn tick(&mut self, delta: f32) {
        self.elapsed_secs = (self.elapsed_secs + delta.max(0.0)).min(self.max_secs);
    }

    /// Put the ability on cooldown iff it is ready; returns whether it fired.
    pub fn trigger(&mut self) -> bool {
        if self.ready() {
            self.elapsed_secs = 0.0;
            true
        } else {
            false
        }
    }

    /// Fraction of the cooldown remaining, in `[0, 1]` (1 == just triggered). Handy
    /// for a HUD radial-sweep overlay.
    pub fn fraction_remaining(&self) -> f32 {
        if self.max_secs <= 0.0 {
            0.0
        } else {
            1.0 - (self.elapsed_secs / self.max_secs).clamp(0.0, 1.0)
        }
    }
}

/// A small stock of ability charges (e.g. a dash with 2 charges). Mirrors
/// `leafwing_abilities::Charges` semantics: spend on use, regenerate over time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Charges {
    current: u8,
    max: u8,
}

impl Charges {
    /// A charge bank that starts full with `max` charges.
    pub fn full(max: u8) -> Self {
        Charges { current: max, max }
    }

    /// How many charges are available right now.
    pub fn current(&self) -> u8 {
        self.current
    }

    /// Spend one charge if any remain; returns whether one was spent.
    pub fn expend(&mut self) -> bool {
        if self.current > 0 {
            self.current -= 1;
            true
        } else {
            false
        }
    }

    /// Add one charge back, saturating at `max` (called when a cooldown completes).
    pub fn replenish(&mut self) {
        if self.current < self.max {
            self.current += 1;
        }
    }
}

/// A regenerating resource pool (mana for the MOBA). Mirrors
/// `leafwing_abilities::Pool`: a bounded current/max value that regenerates over
/// time and is expended by ability use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pool {
    current: f32,
    max: f32,
    regen_per_sec: f32,
}

impl Pool {
    /// A pool that starts full at `max`, regenerating `regen_per_sec` per second.
    pub fn new(max: f32, regen_per_sec: f32) -> Self {
        Pool {
            current: max.max(0.0),
            max: max.max(0.0),
            regen_per_sec: regen_per_sec.max(0.0),
        }
    }

    /// The current amount available.
    pub fn available(&self) -> f32 {
        self.current
    }

    /// The maximum capacity.
    pub fn max(&self) -> f32 {
        self.max
    }

    /// True if at least `cost` is available.
    pub fn can_pay(&self, cost: f32) -> bool {
        self.current >= cost
    }

    /// Spend `cost` iff affordable; returns whether it was paid.
    pub fn expend(&mut self, cost: f32) -> bool {
        if self.can_pay(cost) {
            self.current -= cost;
            true
        } else {
            false
        }
    }

    /// Regenerate over `delta` seconds, saturating at `max`.
    pub fn regen(&mut self, delta: f32) {
        self.current = (self.current + self.regen_per_sec * delta.max(0.0)).min(self.max);
    }
}

/// The local champion's four ability slots (Q/W/E/R). Indexed by [`AbilitySlot`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AbilitySlot(pub usize);

impl AbilitySlot {
    pub const Q: AbilitySlot = AbilitySlot(0);
    pub const W: AbilitySlot = AbilitySlot(1);
    pub const E: AbilitySlot = AbilitySlot(2);
    pub const R: AbilitySlot = AbilitySlot(3);
}

/// The local player's ability book: a cooldown + mana cost per Q/W/E/R slot, plus a
/// shared mana [`Pool`]. This is the client-side analogue of a `leafwing_abilities`
/// `AbilityState` bundle, kept as a Bevy resource for the single local champion.
#[derive(Resource, Clone, Debug)]
pub struct AbilityBook {
    /// Q, W, E, R cooldowns.
    pub cooldowns: [Cooldown; 4],
    /// Q, W, E, R mana costs.
    pub costs: [f32; 4],
    /// The champion's mana pool.
    pub mana: Pool,
}

impl Default for AbilityBook {
    /// A representative default kit: short basic-ability cooldowns, a long ultimate,
    /// and a modest mana pool that regenerates steadily. Tuned for feel, not balance —
    /// the authoritative numbers live in the sim / champion DSL.
    fn default() -> Self {
        AbilityBook {
            cooldowns: [
                Cooldown::from_secs(4.0),  // Q
                Cooldown::from_secs(8.0),  // W
                Cooldown::from_secs(10.0), // E
                Cooldown::from_secs(90.0), // R (ultimate)
            ],
            costs: [40.0, 60.0, 70.0, 100.0],
            mana: Pool::new(300.0, 8.0),
        }
    }
}

impl AbilityBook {
    /// Whether the ability in `slot` can be cast right now (off cooldown AND enough
    /// mana). A pure query — the HUD calls this to decide whether to highlight.
    pub fn can_cast(&self, slot: AbilitySlot) -> bool {
        self.cooldowns[slot.0].ready() && self.mana.can_pay(self.costs[slot.0])
    }

    /// Attempt to cast `slot`: if ready and affordable, spend the mana, start the
    /// cooldown, and return `true` (the caller then emits the sim intent). Otherwise
    /// leave state untouched and return `false`.
    pub fn try_cast(&mut self, slot: AbilitySlot) -> bool {
        if !self.can_cast(slot) {
            return false;
        }
        // Order matters only for the (impossible here) partial-failure case; both
        // preconditions were checked by `can_cast`, so these always succeed.
        let paid = self.mana.expend(self.costs[slot.0]);
        let fired = self.cooldowns[slot.0].trigger();
        paid && fired
    }

    /// Advance every cooldown and regenerate mana by `delta` seconds.
    pub fn tick(&mut self, delta: f32) {
        for cd in &mut self.cooldowns {
            cd.tick(delta);
        }
        self.mana.regen(delta);
    }
}

/// Tick the local ability book each frame from Bevy's `Time`. Registered by
/// [`crate::clientinput::MobaInputPlugin`].
pub fn tick_abilities(mut book: ResMut<AbilityBook>, time: Res<Time>) {
    book.tick(time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_gates_until_elapsed() {
        let mut cd = Cooldown::from_secs(2.0);
        assert!(cd.ready());
        assert!(cd.trigger());
        assert!(!cd.ready());
        assert!(!cd.trigger()); // still on cooldown
        cd.tick(1.0);
        assert!(!cd.ready());
        cd.tick(1.5); // clamps at max
        assert!(cd.ready());
        assert!(cd.trigger());
    }

    #[test]
    fn pool_expend_and_regen() {
        let mut pool = Pool::new(100.0, 10.0);
        assert!(pool.expend(60.0));
        assert_eq!(pool.available(), 40.0);
        assert!(!pool.expend(60.0)); // not enough
        pool.regen(3.0); // +30, capped by max
        assert_eq!(pool.available(), 70.0);
        pool.regen(100.0);
        assert_eq!(pool.available(), pool.max());
    }

    #[test]
    fn charges_expend_and_replenish() {
        let mut ch = Charges::full(2);
        assert!(ch.expend());
        assert!(ch.expend());
        assert!(!ch.expend());
        ch.replenish();
        assert_eq!(ch.current(), 1);
    }

    #[test]
    fn ability_book_cast_requires_cooldown_and_mana() {
        let mut book = AbilityBook::default();
        // Q is ready and affordable at start.
        assert!(book.can_cast(AbilitySlot::Q));
        assert!(book.try_cast(AbilitySlot::Q));
        // Immediately after, Q is on cooldown.
        assert!(!book.can_cast(AbilitySlot::Q));
        assert!(!book.try_cast(AbilitySlot::Q));
        // Drain mana so even a ready ability is refused.
        book.mana = Pool::new(0.0, 0.0);
        assert!(!book.can_cast(AbilitySlot::W));
    }
}

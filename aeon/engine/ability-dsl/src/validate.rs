//! Spec validation — the ONE gate that kills malformed champions.
//!
//! Two layers of validation protect the sim:
//!   1. *Structural* (free, at parse time): serde/RON rejects any unknown [`Effect`]
//!      or [`Targeting`] variant and any unknown struct field. An ability that names
//!      an effect outside the closed vocabulary never even deserializes.
//!   2. *Semantic* (this module): stats must be sane, cooldowns positive, effect
//!      amounts/durations positive, and targeting must be coherent with `range`.
//!
//! A spec that passes [`validate`] is guaranteed safe to compile into the sim.

use crate::spec::{AbilitySpec, ChampionSpec, Effect, Scaling, Targeting};

/// A precise, human-readable reason a champion spec is invalid.
#[derive(Debug, Clone, PartialEq)]
pub enum SpecError {
    /// The RON text could not be parsed, or named an effect/targeting/field outside
    /// the closed vocabulary. Carries the underlying parser message.
    Parse(String),
    /// A base stat is out of range (e.g. non-positive HP).
    Stat(String),
    /// An ability is malformed (bad cooldown, empty effects, incoherent targeting).
    Ability { ability: String, reason: String },
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::Parse(m) => write!(f, "spec parse error: {m}"),
            SpecError::Stat(m) => write!(f, "invalid base stat: {m}"),
            SpecError::Ability { ability, reason } => {
                write!(f, "invalid ability '{ability}': {reason}")
            }
        }
    }
}

impl std::error::Error for SpecError {}

/// Validate a fully-parsed champion spec against the closed vocabulary's semantic
/// rules. Returns the first problem found (fail fast) with a clear message.
pub fn validate(spec: &ChampionSpec) -> Result<(), SpecError> {
    if spec.name.trim().is_empty() {
        return Err(SpecError::Stat("champion name must not be empty".into()));
    }
    validate_stats(spec)?;
    for ability in &spec.abilities {
        validate_ability(ability)?;
    }
    Ok(())
}

fn validate_stats(spec: &ChampionSpec) -> Result<(), SpecError> {
    let b = &spec.base;
    if b.hp <= 0 {
        return Err(SpecError::Stat(format!("hp must be > 0, got {}", b.hp)));
    }
    if b.ad < 0 {
        return Err(SpecError::Stat(format!("ad must be >= 0, got {}", b.ad)));
    }
    if b.armor < 0 {
        return Err(SpecError::Stat(format!(
            "armor must be >= 0, got {}",
            b.armor
        )));
    }
    if !(b.attack_speed.is_finite() && b.attack_speed > 0.0) {
        return Err(SpecError::Stat(format!(
            "attack_speed must be finite and > 0, got {}",
            b.attack_speed
        )));
    }
    if !(b.move_speed.is_finite() && b.move_speed > 0.0) {
        return Err(SpecError::Stat(format!(
            "move_speed must be finite and > 0, got {}",
            b.move_speed
        )));
    }
    if !(b.range.is_finite() && b.range >= 0.0) {
        return Err(SpecError::Stat(format!(
            "range must be finite and >= 0, got {}",
            b.range
        )));
    }
    Ok(())
}

fn validate_ability(a: &AbilitySpec) -> Result<(), SpecError> {
    let err = |reason: String| SpecError::Ability {
        ability: a.name.clone(),
        reason,
    };

    if a.name.trim().is_empty() {
        return Err(SpecError::Ability {
            ability: "<unnamed>".into(),
            reason: "ability name must not be empty".into(),
        });
    }
    if a.cooldown == 0 {
        return Err(err("cooldown must be > 0 ticks".into()));
    }
    if let Some(cost) = a.cost {
        if cost < 0 {
            return Err(err(format!("cost must be >= 0, got {cost}")));
        }
    }
    if a.effects.is_empty() {
        return Err(err("must declare at least one effect".into()));
    }
    // Targeted / ranged abilities need a positive range to be meaningful.
    if matches!(
        a.targeting,
        Targeting::Target | Targeting::Skillshot | Targeting::Area
    ) && !(a.range.is_finite() && a.range > 0.0)
    {
        return Err(err(format!(
            "targeting {:?} requires range > 0, got {}",
            a.targeting, a.range
        )));
    }
    for effect in &a.effects {
        validate_effect(a, effect)?;
    }
    Ok(())
}

fn validate_effect(a: &AbilitySpec, effect: &Effect) -> Result<(), SpecError> {
    let err = |reason: String| SpecError::Ability {
        ability: a.name.clone(),
        reason,
    };
    match effect {
        Effect::Damage { amount, scaling } => {
            if *amount < 0 {
                return Err(err(format!("Damage amount must be >= 0, got {amount}")));
            }
            if let Some(Scaling::AttackDamage { ratio }) = scaling {
                if !(ratio.is_finite() && *ratio >= 0.0) {
                    return Err(err(format!(
                        "Damage scaling ratio must be finite and >= 0, got {ratio}"
                    )));
                }
            }
            // A Damage effect that does nothing (0 amount, no scaling) is useless.
            if *amount == 0 && scaling.is_none() {
                return Err(err(
                    "Damage with amount 0 and no scaling has no effect".into()
                ));
            }
        }
        Effect::Heal { amount } => {
            if *amount <= 0 {
                return Err(err(format!("Heal amount must be > 0, got {amount}")));
            }
        }
        Effect::Slow { duration } => {
            if *duration == 0 {
                return Err(err("Slow duration must be > 0 ticks".into()));
            }
        }
        Effect::Stun { duration } => {
            if *duration == 0 {
                return Err(err("Stun duration must be > 0 ticks".into()));
            }
        }
        Effect::Dash { distance } => {
            if !(distance.is_finite() && *distance > 0.0) {
                return Err(err(format!(
                    "Dash distance must be finite and > 0, got {distance}"
                )));
            }
        }
    }
    Ok(())
}

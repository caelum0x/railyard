//! Loki — ASSASSIN (Norse, dash burst). Graduated from the Aeonrift min-ECS DSL
//! onto `lol_core` (see `MAP.md`). DSL: `aeonrift/content/champions/loki.ron`.
//!
//! - Q Shadow Step    : `ActionDash` (`Pointer`) + `ActionDamage` (`Nearest`) + `ActionDisplace` (`Slow`)
//! - W Serpent's Venom: `ActionDamage` (`Rectangle`) + `ActionDisplace` (`Slow`)
//! - E Vanishing Trick: `BuffSelfHeal`

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1, ANIM_SPELL2, ANIM_SPELL3};
use lol_base::render_cmd::CommandAnimationPlay;
use lol_core::action::damage::{
    ActionDamage, ActionDamageEffect, DamageShape, TargetDamage, TargetFilter,
};
use lol_core::action::dash::{ActionDash, DashMoveType};
use lol_core::action::displace::{
    ActionDisplace, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::base::buff::BuffOf;
use lol_core::buffs::common_buffs::BuffSelfHeal;
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

const DASH_SPEED: f32 = 1400.0;
const WIDTH: f32 = 120.0;
// Q Shadow Step — Target, DSL range/dash 16, Slow 10.
const Q_DASH_MAX: f32 = 480.0;
const Q_RANGE: f32 = 480.0;
const Q_SLOW_PCT: f32 = 0.30;
const Q_SLOW_DUR: f32 = 1.0;
// W Serpent's Venom — Skillshot, DSL range 20, Slow 16.
const W_LENGTH: f32 = 600.0;
const W_RANGE: f32 = 600.0;
const W_SLOW_PCT: f32 = 0.30;
const W_SLOW_DUR: f32 = 1.6;
// E Vanishing Trick — SelfCast, Heal 70.
const E_HEAL: f32 = 70.0;

#[derive(Default)]
pub struct PluginLoki;

impl Plugin for PluginLoki {
    fn build(&self, app: &mut App) {
        app.add_observer(on_loki_q);
        app.add_observer(on_loki_w);
        app.add_observer(on_loki_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Loki"))]
#[reflect(Component)]
pub struct Loki;

fn on_loki_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Loki>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_champ.get(entity).is_err() {
        return;
    }
    let Ok(skill) = q_skill.get(trigger.skill_entity) else {
        return;
    };
    if !matches!(skill.slot, SkillSlot::Q) {
        return;
    }
    let skill_spell = skill.spell.clone();
    let point = trigger.point;
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL1.to_string(),
        repeat: false,
        duration: None,
    });
    commands.trigger(ActionDash {
        entity,
        point,
        move_type: DashMoveType::Pointer { max: Q_DASH_MAX },
        speed: DASH_SPEED,
    });
    commands.trigger(ActionDamage {
        entity,
        skill: skill_spell,
        effects: vec![ActionDamageEffect {
            shape: DamageShape::Nearest { max_distance: Q_RANGE },
            damage_list: vec![TargetDamage {
                filter: TargetFilter::All,
                amount: "q_damage".to_string(),
                damage_type: DamageType::Physical,
                ..Default::default()
            }],
            ..Default::default()
        }],
    });
    commands.trigger(ActionDisplace {
        entity,
        targets: DisplaceTargetSelection::Nearest { range: Q_RANGE },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow {
            percent: Q_SLOW_PCT,
            duration: Q_SLOW_DUR,
        }],
        cone_hit_policy: None,
    });
}

fn on_loki_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Loki>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_champ.get(entity).is_err() {
        return;
    }
    let Ok(skill) = q_skill.get(trigger.skill_entity) else {
        return;
    };
    if !matches!(skill.slot, SkillSlot::W) {
        return;
    }
    let skill_spell = skill.spell.clone();
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL2.to_string(),
        repeat: false,
        duration: None,
    });
    commands.trigger(ActionDamage {
        entity,
        skill: skill_spell,
        effects: vec![ActionDamageEffect {
            shape: DamageShape::Rectangle {
                width: WIDTH,
                length: W_LENGTH,
                start_distance: 0.0,
            },
            damage_list: vec![TargetDamage {
                filter: TargetFilter::All,
                amount: "w_damage".to_string(),
                damage_type: DamageType::Physical,
                ..Default::default()
            }],
            ..Default::default()
        }],
    });
    commands.trigger(ActionDisplace {
        entity,
        targets: DisplaceTargetSelection::Nearest { range: W_RANGE },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow {
            percent: W_SLOW_PCT,
            duration: W_SLOW_DUR,
        }],
        cone_hit_policy: None,
    });
}

fn on_loki_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Loki>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_champ.get(entity).is_err() {
        return;
    }
    let Ok(skill) = q_skill.get(trigger.skill_entity) else {
        return;
    };
    if !matches!(skill.slot, SkillSlot::E) {
        return;
    }
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL3.to_string(),
        repeat: false,
        duration: None,
    });
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffSelfHeal::new(E_HEAL));
}

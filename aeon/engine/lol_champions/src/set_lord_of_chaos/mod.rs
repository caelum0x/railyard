//! Set, Lord of Chaos — ASSASSIN (Egyptian, dive/escape). Graduated from the
//! Aeonrift min-ECS DSL onto `lol_core` (see `MAP.md`).
//! DSL: `aeonrift/content/champions/set-lord-of-chaos.ron`.
//!
//! - Q Desert Ambush: `ActionDash` (`Pointer`) + `ActionDamage` (`Nearest`)
//! - W Sandstorm    : `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`)
//! - E Chaos Step   : `ActionDash` (`Pointer`) + `BuffSelfHeal`

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
    ActionDisplace, DisplaceCenter, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::base::buff::BuffOf;
use lol_core::buffs::common_buffs::BuffSelfHeal;
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

const DASH_SPEED: f32 = 1400.0;
// Q Desert Ambush — Target, DSL range/dash 18.
const Q_DASH_MAX: f32 = 540.0;
const Q_RANGE: f32 = 540.0;
// W Sandstorm — Area, DSL range 14, Slow 14.
const W_RADIUS: f32 = 420.0;
const W_SLOW_PCT: f32 = 0.30;
const W_SLOW_DUR: f32 = 1.4;
// E Chaos Step — SelfCast, Dash 12, Heal 60.
const E_DASH_MAX: f32 = 360.0;
const E_HEAL: f32 = 60.0;

#[derive(Default)]
pub struct PluginSet;

impl Plugin for PluginSet {
    fn build(&self, app: &mut App) {
        app.add_observer(on_set_q);
        app.add_observer(on_set_w);
        app.add_observer(on_set_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Set"))]
#[reflect(Component)]
pub struct Set;

fn on_set_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Set>>,
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
}

fn on_set_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Set>>,
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
            shape: DamageShape::Circle { radius: W_RADIUS },
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
        targets: DisplaceTargetSelection::Circle {
            radius: W_RADIUS,
            center: DisplaceCenter::Caster,
        },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow {
            percent: W_SLOW_PCT,
            duration: W_SLOW_DUR,
        }],
        cone_hit_policy: None,
    });
}

fn on_set_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Set>>,
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
    let point = trigger.point;
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL3.to_string(),
        repeat: false,
        duration: None,
    });
    commands.trigger(ActionDash {
        entity,
        point,
        move_type: DashMoveType::Pointer { max: E_DASH_MAX },
        speed: DASH_SPEED,
    });
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffSelfHeal::new(E_HEAL));
}

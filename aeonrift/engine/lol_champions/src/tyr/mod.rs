//! Tyr — TANK (Norse, lockdown/sustain). Graduated from the Aeonrift min-ECS DSL
//! onto `lol_core` (see `MAP.md`). DSL: `aeonrift/content/champions/tyr.ron`.
//!
//! - Q Oath of the Bound  : `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`)
//! - W Warden's Toll       : `ActionDamage` (`Circle`)  + `ActionDisplace` (`Slow`)
//! - E Unyielding Sacrifice: `BuffSelfHeal`

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1, ANIM_SPELL2, ANIM_SPELL3};
use lol_base::render_cmd::CommandAnimationPlay;
use lol_core::action::damage::{
    ActionDamage, ActionDamageEffect, DamageShape, TargetDamage, TargetFilter,
};
use lol_core::action::displace::{
    ActionDisplace, DisplaceCenter, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::base::buff::BuffOf;
use lol_core::buffs::common_buffs::BuffSelfHeal;
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

// Q Oath of the Bound — Target, DSL range 12, Stun 24.
const Q_RANGE: f32 = 360.0;
const Q_STUN: f32 = 2.4;
// W Warden's Toll — Area, DSL range 10, Slow 30.
const W_RADIUS: f32 = 300.0;
const W_SLOW_PCT: f32 = 0.30;
const W_SLOW_DUR: f32 = 3.0;
// E Unyielding Sacrifice — SelfCast, Heal 160.
const E_HEAL: f32 = 160.0;

#[derive(Default)]
pub struct PluginTyr;

impl Plugin for PluginTyr {
    fn build(&self, app: &mut App) {
        app.add_observer(on_tyr_q);
        app.add_observer(on_tyr_w);
        app.add_observer(on_tyr_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Tyr"))]
#[reflect(Component)]
pub struct Tyr;

fn on_tyr_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Tyr>>,
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
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL1.to_string(),
        repeat: false,
        duration: None,
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
        effects: vec![DisplaceEffect::Stun { duration: Q_STUN }],
        cone_hit_policy: None,
    });
}

fn on_tyr_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Tyr>>,
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

fn on_tyr_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Tyr>>,
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

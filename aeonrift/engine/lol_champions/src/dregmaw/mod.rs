//! Dregmaw, the Duskbulwark — TANK (Umbral, frontline). Graduated from the
//! Aeonrift min-ECS DSL onto `lol_core` (see `MAP.md`).
//! DSL: `aeonrift/content/champions/dregmaw.ron`.
//!
//! - Q Umbral Seize: `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`)
//! - W Eclipse Skin: `BuffSelfHeal`
//! - E Duskwave    : `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`)

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

// Q Umbral Seize — Target, DSL range 9, Stun 45.
const Q_RANGE: f32 = 270.0;
const Q_STUN: f32 = 4.5;
// W Eclipse Skin — SelfCast, Heal 105.
const W_HEAL: f32 = 105.0;
// E Duskwave — Area, DSL range 11, Slow 90.
const E_RADIUS: f32 = 330.0;
const E_SLOW_PCT: f32 = 0.30;
const E_SLOW_DUR: f32 = 9.0;

#[derive(Default)]
pub struct PluginDregmaw;

impl Plugin for PluginDregmaw {
    fn build(&self, app: &mut App) {
        app.add_observer(on_dregmaw_q);
        app.add_observer(on_dregmaw_w);
        app.add_observer(on_dregmaw_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Dregmaw"))]
#[reflect(Component)]
pub struct Dregmaw;

fn on_dregmaw_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Dregmaw>>,
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

fn on_dregmaw_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Dregmaw>>,
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
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL2.to_string(),
        repeat: false,
        duration: None,
    });
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffSelfHeal::new(W_HEAL));
}

fn on_dregmaw_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Dregmaw>>,
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
    let skill_spell = skill.spell.clone();
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL3.to_string(),
        repeat: false,
        duration: None,
    });
    commands.trigger(ActionDamage {
        entity,
        skill: skill_spell,
        effects: vec![ActionDamageEffect {
            shape: DamageShape::Circle { radius: E_RADIUS },
            damage_list: vec![TargetDamage {
                filter: TargetFilter::All,
                amount: "e_damage".to_string(),
                damage_type: DamageType::Physical,
                ..Default::default()
            }],
            ..Default::default()
        }],
    });
    commands.trigger(ActionDisplace {
        entity,
        targets: DisplaceTargetSelection::Circle {
            radius: E_RADIUS,
            center: DisplaceCenter::Caster,
        },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow {
            percent: E_SLOW_PCT,
            duration: E_SLOW_DUR,
        }],
        cone_hit_policy: None,
    });
}

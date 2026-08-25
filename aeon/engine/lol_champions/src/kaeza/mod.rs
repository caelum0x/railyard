//! Kaeza, the Emberlock — MARKSMAN (Ember, ranged DPS). Graduated from the
//! Aeonrift min-ECS DSL onto `lol_core` (see `MAP.md`).
//! DSL: `aeonrift/content/champions/kaeza.ron`.
//!
//! - Q Scorch Bolt: `ActionDamage` (`Rectangle`)
//! - W Ashstep    : `ActionDash` (`Pointer`)
//! - E Slagshot   : `ActionDamage` (`Nearest`) + `ActionDisplace` (`Slow`)

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
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

const DASH_SPEED: f32 = 1400.0;
const WIDTH: f32 = 120.0;
// Q Scorch Bolt — Skillshot, DSL range 30.
const Q_LENGTH: f32 = 900.0;
// W Ashstep — Skillshot dash, DSL dash 14.
const W_DASH_MAX: f32 = 420.0;
// E Slagshot — Target, DSL range 26, Slow 60.
const E_RANGE: f32 = 780.0;
const E_SLOW_PCT: f32 = 0.30;
const E_SLOW_DUR: f32 = 6.0;

#[derive(Default)]
pub struct PluginKaeza;

impl Plugin for PluginKaeza {
    fn build(&self, app: &mut App) {
        app.add_observer(on_kaeza_q);
        app.add_observer(on_kaeza_w);
        app.add_observer(on_kaeza_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Kaeza"))]
#[reflect(Component)]
pub struct Kaeza;

fn on_kaeza_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Kaeza>>,
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
            shape: DamageShape::Rectangle {
                width: WIDTH,
                length: Q_LENGTH,
                start_distance: 0.0,
            },
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

fn on_kaeza_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Kaeza>>,
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
    let point = trigger.point;
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL2.to_string(),
        repeat: false,
        duration: None,
    });
    commands.trigger(ActionDash {
        entity,
        point,
        move_type: DashMoveType::Pointer { max: W_DASH_MAX },
        speed: DASH_SPEED,
    });
}

fn on_kaeza_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Kaeza>>,
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
            shape: DamageShape::Nearest { max_distance: E_RANGE },
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
        targets: DisplaceTargetSelection::Nearest { range: E_RANGE },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow {
            percent: E_SLOW_PCT,
            duration: E_SLOW_DUR,
        }],
        cone_hit_policy: None,
    });
}

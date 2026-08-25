//! Zeus, King of Olympus — BURST MAGE (Greek). Graduated from the Aeonrift
//! min-ECS DSL onto the real `lol_core` vocabulary (see `MAP.md`).
//!
//! Kit (composed ONLY from the closed `lol_core` action/buff set):
//! - Q Thunderbolt  : `ActionDamage` (`Rectangle`, Magic) + `ActionDisplace` (`Stun`)
//! - W Storm's Wrath: `ActionDamage` (`Circle`, Magic)   + `ActionDisplace` (`Slow`)
//!
//! DSL source: `aeonrift/content/champions/zeus.ron`.

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1, ANIM_SPELL2};
use lol_base::render_cmd::CommandAnimationPlay;
use lol_core::action::damage::{
    ActionDamage, ActionDamageEffect, DamageShape, TargetDamage, TargetFilter,
};
use lol_core::action::displace::{
    ActionDisplace, DisplaceCenter, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

// Q Thunderbolt — Skillshot, DSL range 30, Stun 14.
const Q_WIDTH: f32 = 120.0;
const Q_LENGTH: f32 = 900.0; // 30 * 30
const Q_RANGE: f32 = 900.0;
const Q_STUN: f32 = 1.4; // 14 * 0.1
// W Storm's Wrath — Area, DSL range 12, Slow 30.
const W_RADIUS: f32 = 360.0; // 12 * 30
const W_SLOW_PCT: f32 = 0.30;
const W_SLOW_DUR: f32 = 3.0; // 30 * 0.1

#[derive(Default)]
pub struct PluginZeus;

impl Plugin for PluginZeus {
    fn build(&self, app: &mut App) {
        app.add_observer(on_zeus_q);
        app.add_observer(on_zeus_w);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Zeus"))]
#[reflect(Component)]
pub struct Zeus;

/// Q — Thunderbolt: a line bolt that deals Magic damage and stuns the first foe.
fn on_zeus_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Zeus>>,
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
                width: Q_WIDTH,
                length: Q_LENGTH,
                start_distance: 0.0,
            },
            damage_list: vec![TargetDamage {
                filter: TargetFilter::All,
                amount: "q_damage".to_string(),
                damage_type: DamageType::Magic,
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

/// W — Storm's Wrath: a storm erupts around Zeus, Magic damage + slow to all nearby.
fn on_zeus_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Zeus>>,
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
                damage_type: DamageType::Magic,
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

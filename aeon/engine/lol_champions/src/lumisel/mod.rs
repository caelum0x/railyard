//! Lumisel — SUPPORT (Tide, enchanter). Graduated from the Aeonrift min-ECS DSL
//! onto `lol_core` (see `MAP.md`). DSL: `aeonrift/content/champions/lumisel.ron`.
//!
//! - Q Tidal Blessing : `BuffSelfHeal`
//! - W Frostwater Veil: `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`)
//! - E Riptide Ward   : `BuffSelfHeal`

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

// Q Tidal Blessing — SelfCast, Heal 105.
const Q_HEAL: f32 = 105.0;
// W Frostwater Veil — Area, DSL range 11, Slow 90.
const W_RADIUS: f32 = 330.0;
const W_SLOW_PCT: f32 = 0.30;
const W_SLOW_DUR: f32 = 9.0;
// E Riptide Ward — SelfCast, Heal 50.
const E_HEAL: f32 = 50.0;

#[derive(Default)]
pub struct PluginLumisel;

impl Plugin for PluginLumisel {
    fn build(&self, app: &mut App) {
        app.add_observer(on_lumisel_q);
        app.add_observer(on_lumisel_w);
        app.add_observer(on_lumisel_e);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Lumisel"))]
#[reflect(Component)]
pub struct Lumisel;

fn on_lumisel_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Lumisel>>,
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
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL1.to_string(),
        repeat: false,
        duration: None,
    });
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffSelfHeal::new(Q_HEAL));
}

fn on_lumisel_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Lumisel>>,
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

fn on_lumisel_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Lumisel>>,
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

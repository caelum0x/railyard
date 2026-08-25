//! Pyromancer — auto-ported from the Aeonrift min-ECS DSL onto `lol_core`
//! via the proven translation map. DSL source:
//! `aeonrift/content/champions/`. One Bevy `Plugin`; Q/W/E/R observers
//! compose closed-vocabulary `lol_core` actions/buffs. Asset-free.

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1};
use lol_base::render_cmd::CommandAnimationPlay;
use lol_core::action::damage::{
    ActionDamage, ActionDamageEffect, DamageShape, TargetDamage, TargetFilter,
};
use lol_core::action::displace::{
    ActionDisplace, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::damage::DamageType;
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

#[derive(Default)]
pub struct PluginPyromancer;

impl Plugin for PluginPyromancer {
    fn build(&self, app: &mut App) {
        app.add_observer(on_pyromancer_q);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Pyromancer"))]
#[reflect(Component)]
pub struct Pyromancer;

fn on_pyromancer_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Pyromancer>>,
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
            shape: DamageShape::Rectangle { width: 120.0, length: 840.0, start_distance: 0.0 },
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
        targets: DisplaceTargetSelection::Nearest { range: 840.0 },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Stun { duration: 1.6 }],
        cone_hit_policy: None,
    });
}


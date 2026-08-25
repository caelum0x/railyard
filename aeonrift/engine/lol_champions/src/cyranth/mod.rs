//! Cyranth — auto-ported from the Aeonrift min-ECS DSL onto `lol_core`
//! via the proven translation map. DSL source:
//! `aeonrift/content/champions/`. One Bevy `Plugin`; Q/W/E/R observers
//! compose closed-vocabulary `lol_core` actions/buffs. Asset-free.

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1, ANIM_SPELL2};
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
pub struct PluginCyranth;

impl Plugin for PluginCyranth {
    fn build(&self, app: &mut App) {
        app.add_observer(on_cyranth_q);
        app.add_observer(on_cyranth_w);
    }
}

#[derive(Component, Reflect, Default)]
#[require(Champion, Name = Name::new("Cyranth"))]
#[reflect(Component)]
pub struct Cyranth;

fn on_cyranth_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Cyranth>>,
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
            shape: DamageShape::Rectangle { width: 120.0, length: 660.0, start_distance: 0.0 },
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

fn on_cyranth_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_champ: Query<(), With<Cyranth>>,
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
            shape: DamageShape::Nearest { max_distance: 600.0 },
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
        targets: DisplaceTargetSelection::Nearest { range: 600.0 },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Slow { percent: 0.3, duration: 7.5 }],
        cone_hit_policy: None,
    });
}


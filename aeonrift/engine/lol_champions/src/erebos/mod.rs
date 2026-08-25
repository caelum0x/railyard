//! Erebos, the Eclipse Warden — 原创英雄 (ORIGINAL IP)。
//!
//! 完全由 `lol_core` 的封闭动作/效果/buff 词汇表组合而成，不依赖任何 Riot 资产即可运行。
//! 遵循 `ahri/mod.rs` 的模式：一个英雄 = 一个 Bevy `Plugin`，通过 `add_observer`
//! 注册各技能的 `EventSkillCast` 处理器，每个处理器 compose 共享动作。
//!
//! 词汇来源：
//! - Q Shadowlance : `ActionDamage` + `DamageShape::Rectangle` (直线技能)
//! - W Veil of Dusk: `BuffMoveSpeed` + `BuffSelfHeal` (自身增益)
//! - E Umbral Step : `ActionDash` + `DashMoveType::Pointer` (位移)
//! - R Eclipse     : `ActionDamage` (圆形) + `ActionDisplace` (`DisplaceEffect::Stun`)
//! - 被动          : 命中挂载 `BuffEclipseMark`

pub mod buffs;

#[cfg(test)]
mod tests;

use bevy::prelude::*;
use lol_base::animation_names::{ANIM_SPELL1, ANIM_SPELL2, ANIM_SPELL3, ANIM_SPELL4};
use lol_base::render_cmd::CommandAnimationPlay;
use lol_core::action::damage::{
    ActionDamage, ActionDamageEffect, DamageShape, TargetDamage, TargetFilter,
};
use lol_core::action::dash::{ActionDash, DashMoveType};
use lol_core::action::displace::{
    ActionDisplace, DisplaceCenter, DisplaceEffect, DisplaceMotion, DisplaceTargetSelection,
};
use lol_core::base::buff::BuffOf;
use lol_core::buffs::common_buffs::{BuffMoveSpeed, BuffSelfHeal};
use lol_core::damage::{DamageType, EventDamageCreate};
use lol_core::entities::champion::Champion;
use lol_core::skill::{EventSkillCast, Skill, SkillSlot};

use crate::erebos::buffs::{BuffEclipseMark, BuffUmbralVeil};

/// Q 蚀刻长矛：直线宽度（世界单位）。
const Q_WIDTH: f32 = 120.0;
/// Q 蚀刻长矛：直线长度（世界单位）。
const Q_LENGTH: f32 = 900.0;
/// W 暮纱：移速加成（30%）。
const W_MOVE_SPEED_BONUS: f32 = 0.30;
/// W 暮纱：持续时间（秒）。
const W_DURATION: f32 = 2.5;
/// W 暮纱：治疗量（原创基线，与 tuning.ron rank1 对齐）。
const W_HEAL_AMOUNT: f32 = 60.0;
/// E 暗影步：最大位移距离。
const E_MAX_RANGE: f32 = 450.0;
/// E 暗影步：位移速度。
const E_SPEED: f32 = 1400.0;
/// R 日蚀：作用半径。
const R_RADIUS: f32 = 550.0;
/// R 日蚀：眩晕时长（秒）。
const R_STUN_DURATION: f32 = 1.25;
/// 被动蚀刻印记：持续时间（秒）。
const MARK_DURATION: f32 = 3.0;

#[derive(Default)]
pub struct PluginErebos;

impl Plugin for PluginErebos {
    fn build(&self, app: &mut App) {
        app.add_observer(on_erebos_q);
        app.add_observer(on_erebos_w);
        app.add_observer(on_erebos_e);
        app.add_observer(on_erebos_r);
        app.add_observer(on_erebos_damage_hit);
    }
}

#[derive(Component, Reflect)]
#[require(Champion, Name = Name::new("Erebos"))]
#[reflect(Component)]
pub struct Erebos;

/// Q — Shadowlance：向前方发出一道暗影长矛，对直线上的敌人造成魔法伤害。
fn on_erebos_q(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_erebos: Query<(), With<Erebos>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_erebos.get(entity).is_err() {
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
}

/// W — Veil of Dusk：披上暮色之纱，获得移速并治疗自身。
fn on_erebos_w(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_erebos: Query<(), With<Erebos>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_erebos.get(entity).is_err() {
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
        .with_related::<BuffOf>(BuffUmbralVeil::new(W_DURATION));
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffMoveSpeed::new(W_MOVE_SPEED_BONUS, W_DURATION));
    commands
        .entity(entity)
        .with_related::<BuffOf>(BuffSelfHeal::new(W_HEAL_AMOUNT));
}

/// E — Umbral Step：朝目标点瞬闪一段距离（超出上限则钳制到上限）。
fn on_erebos_e(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_erebos: Query<(), With<Erebos>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_erebos.get(entity).is_err() {
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
        move_type: DashMoveType::Pointer { max: E_MAX_RANGE },
        speed: E_SPEED,
    });
}

/// R — Eclipse：周身爆发黑暗，对范围内敌方英雄造成魔法伤害并眩晕。
fn on_erebos_r(
    trigger: On<EventSkillCast>,
    mut commands: Commands,
    q_erebos: Query<(), With<Erebos>>,
    q_skill: Query<&Skill>,
) {
    let entity = trigger.event_target();
    if q_erebos.get(entity).is_err() {
        return;
    }
    let Ok(skill) = q_skill.get(trigger.skill_entity) else {
        return;
    };
    if !matches!(skill.slot, SkillSlot::R) {
        return;
    }

    let skill_spell = skill.spell.clone();
    commands.trigger(CommandAnimationPlay {
        entity,
        hash: ANIM_SPELL4.to_string(),
        repeat: false,
        duration: None,
    });

    // 魔法伤害（圆形，仅英雄）
    commands.trigger(ActionDamage {
        entity,
        skill: skill_spell,
        effects: vec![ActionDamageEffect {
            shape: DamageShape::Circle { radius: R_RADIUS },
            damage_list: vec![TargetDamage {
                filter: TargetFilter::Champion,
                amount: "r_damage".to_string(),
                damage_type: DamageType::Magic,
                ..Default::default()
            }],
            ..Default::default()
        }],
    });

    // 眩晕（圆形，以自身为中心，仅施加效果不位移）
    commands.trigger(ActionDisplace {
        entity,
        targets: DisplaceTargetSelection::Circle {
            radius: R_RADIUS,
            center: DisplaceCenter::Caster,
        },
        motion: DisplaceMotion::None,
        effects: vec![DisplaceEffect::Stun {
            duration: R_STUN_DURATION,
        }],
        cone_hit_policy: None,
    });
}

/// 被动 — Umbral Gathering：Erebos 造成技能伤害时为目标挂载蚀刻印记。
fn on_erebos_damage_hit(
    trigger: On<EventDamageCreate>,
    mut commands: Commands,
    q_erebos: Query<(), With<Erebos>>,
) {
    let source = trigger.source;
    if q_erebos.get(source).is_err() {
        return;
    }

    let target = trigger.event_target();
    commands
        .entity(target)
        .with_related::<BuffOf>(BuffEclipseMark::new(MARK_DURATION));
}

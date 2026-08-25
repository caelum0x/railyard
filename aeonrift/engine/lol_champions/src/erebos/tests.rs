#![cfg(test)]
//! Erebos 的无头 (headless) 确定性测试。
//!
//! 复用 `crates/lol_core/src/skill/tests.rs` 的 `ActionSkillHarness` 模式：在内存里
//! 构造 `Spell` 资产（**不加载任何 Riot 文件**），生成 Erebos 与假人，直接触发
//! `EventSkillCast` 驱动我们的技能观察者，断言结果确定且可复现。

use std::collections::BTreeMap;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use lol_base::hash_key::HashKey;
use lol_base::spell::{DataSpell, Spell, ValuesEffect};
use lol_base::spell_calc::{
    CalculationPart, CalculationPartEffectValue, CalculationSpell, CalculationType,
};
use lol_core::action::PluginAction;
use lol_core::base::buff::Buffs;
use lol_core::buffs::cc_debuffs::{DebuffStun, PluginCc};
use lol_core::buffs::common_buffs::PluginCommonBuffs;
use lol_core::cooldown::PluginCooldown;
use lol_core::damage::PluginDamage;
use lol_core::entities::champion::Champion;
use lol_core::life::{Health, PluginLife};
use lol_core::movement::Movement;
use lol_core::skill::{CoolDown, EventSkillCast, PluginSkill, Skill, SkillOf, SkillSlot, Skills};
use lol_core::team::Team;

use crate::erebos::PluginErebos;
use crate::erebos::buffs::BuffEclipseMark;

const TEST_FPS: f64 = 30.0;
const Q_SPELL_KEY: u32 = 0xE12B_0001;
const W_SPELL_KEY: u32 = 0xE12B_0002;
const R_SPELL_KEY: u32 = 0xE12B_0004;
const EPSILON: f32 = 1e-4;

fn spell_handle(key: u32) -> Handle<Spell> {
    Handle::from(HashKey::<Spell>::from(key))
}

/// 构造最小无头 app：仅 Erebos 技能所需的核心插件，无 Riot 资产依赖。
fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(AssetPlugin::default());
    app.add_plugins(PluginAction);
    app.add_plugins(PluginCooldown);
    app.add_plugins(PluginDamage);
    app.add_plugins(PluginLife);
    app.add_plugins(PluginSkill);
    app.add_plugins(PluginCommonBuffs);
    app.add_plugins(PluginCc);
    app.add_plugins(PluginErebos);
    app.init_asset::<Spell>();
    app.insert_resource(Time::<Fixed>::from_hz(TEST_FPS));
    app.insert_resource(TimeUpdateStrategy::FixedTimesteps(1));
    app
}

/// 在内存中注册一个技能：单一 `calc_key` 计算键，逐级取自 effect_amounts。
fn insert_spell(app: &mut App, key: u32, calc_key: &str, base: f32) {
    let mut calculations = BTreeMap::new();
    calculations.insert(
        calc_key.to_string(),
        CalculationType::CalculationSpell(CalculationSpell {
            formula_parts: Some(vec![CalculationPart::CalculationPartEffectValue(
                CalculationPartEffectValue {
                    effect_index: Some(1),
                },
            )]),
            multiplier: None,
            precision: None,
        }),
    );
    let effect_values = ValuesEffect {
        values: Some(vec![base; 6]),
    };
    app.world_mut().resource_mut::<Assets<Spell>>().insert(
        spell_handle(key).id(),
        Spell {
            spell_data: Some(DataSpell {
                calculations: Some(calculations),
                effect_amounts: Some(vec![effect_values]),
                mana: Some(vec![0.0; 6]),
                ..Default::default()
            }),
        },
    );
}

/// 生成 Erebos 施法者（`Transform::default()` 面朝 -Z）。
fn spawn_erebos(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            crate::erebos::Erebos,
            Team::Order,
            Transform::default(),
            Skills::default(),
            Movement { speed: 335.0 },
            Health::new(1000.0),
        ))
        .id()
}

/// 为施法者添加一个已就绪（无冷却）的技能实体，返回其 Entity。
fn add_skill(app: &mut App, caster: Entity, slot: SkillSlot, spell_key: u32) -> Entity {
    let mut timer = Timer::from_seconds(3.0, TimerMode::Once);
    timer.tick(Duration::from_secs_f32(3.0));

    let skill_entity = app
        .world_mut()
        .spawn((
            SkillOf(caster),
            Skill::new(slot, spell_handle(spell_key)).with_level(1),
            CoolDown {
                duration: 3.0,
                timer: Some(timer),
            },
        ))
        .id();

    // 关系钩子可能已自动登记；此处幂等补登，确保 caster.Skills 恰好包含一次。
    if let Some(mut skills) = app.world_mut().get_mut::<Skills>(caster) {
        if !skills.iter().any(|e| e == skill_entity) {
            skills.push(skill_entity);
        }
    }
    skill_entity
}

/// 直接触发 `EventSkillCast`（绕过法力/冷却检查，专测技能观察者本身）并推进一帧。
fn cast(app: &mut App, caster: Entity, skill_entity: Entity, index: usize, point: Vec2) {
    app.world_mut()
        .entity_mut(caster)
        .trigger(|e| EventSkillCast {
            entity: e,
            skill_entity,
            index,
            point,
        });
    app.update();
}

fn has_buff<T: Component>(app: &App, entity: Entity) -> bool {
    app.world()
        .get::<Buffs>(entity)
        .map(|b| b.iter().any(|e| app.world().get::<T>(e).is_some()))
        .unwrap_or(false)
}

/// Q 命中直线上的假人，造成恰好 `q_damage` 的魔法伤害，并挂载蚀刻印记。
#[test]
fn erebos_q_damages_and_marks_target_in_line() {
    let mut app = build_app();
    insert_spell(&mut app, Q_SPELL_KEY, "q_damage", 70.0);
    let caster = spawn_erebos(&mut app);
    let q = add_skill(&mut app, caster, SkillSlot::Q, Q_SPELL_KEY);

    // 假人置于施法者正前方（默认 -Z），落在 Q 的矩形内。
    let enemy = app
        .world_mut()
        .spawn((
            Team::Chaos,
            Transform::from_xyz(0.0, 0.0, -300.0),
            Health::new(1000.0),
        ))
        .id();

    cast(&mut app, caster, q, 0, Vec2::new(0.0, -300.0));
    // ActionDamage -> CommandDamageCreate -> EventDamageCreate -> 挂印记，跨多次命令刷新。
    for _ in 0..4 {
        app.update();
    }

    let hp = app.world().get::<Health>(enemy).unwrap().value;
    assert!(
        (hp - 930.0).abs() < EPSILON,
        "Q 应造成 70 点魔法伤害 (1000 -> 930)，实际 {hp}"
    );
    assert!(
        has_buff::<BuffEclipseMark>(&app, enemy),
        "Q 命中应为目标挂载蚀刻印记 (BuffEclipseMark)"
    );
}

/// 确定性：相同种子/输入两次运行 -> bit 级一致的结果（游戏路径无 RNG）。
#[test]
fn erebos_q_is_deterministic_across_runs() {
    fn run_once(base: f32) -> f32 {
        let mut app = build_app();
        insert_spell(&mut app, Q_SPELL_KEY, "q_damage", base);
        let caster = spawn_erebos(&mut app);
        let q = add_skill(&mut app, caster, SkillSlot::Q, Q_SPELL_KEY);
        let enemy = app
            .world_mut()
            .spawn((
                Team::Chaos,
                Transform::from_xyz(0.0, 0.0, -300.0),
                Health::new(1000.0),
            ))
            .id();
        cast(&mut app, caster, q, 0, Vec2::new(0.0, -300.0));
        for _ in 0..4 {
            app.update();
        }
        app.world().get::<Health>(enemy).unwrap().value
    }

    let a = run_once(123.0);
    let b = run_once(123.0);
    assert_eq!(
        a.to_bits(),
        b.to_bits(),
        "同输入两次运行应得 bit 级一致的血量：{a} vs {b}"
    );
    assert!((a - 877.0).abs() < EPSILON, "1000 - 123 = 877，实际 {a}");
}

/// W 为自身施加移速增益（30%）。
#[test]
fn erebos_w_grants_self_move_speed() {
    let mut app = build_app();
    let caster = spawn_erebos(&mut app);
    // W 观察者只施加 buff，不读取 spell 计算，但技能实体仍需存在。
    let w = add_skill(&mut app, caster, SkillSlot::W, W_SPELL_KEY);

    let before = app.world().get::<Movement>(caster).unwrap().speed;
    cast(&mut app, caster, w, 1, Vec2::ZERO);
    for _ in 0..3 {
        app.update();
    }
    let after = app.world().get::<Movement>(caster).unwrap().speed;
    assert!(
        (after - before * 1.30).abs() < 1e-1,
        "W 应给予 30% 移速：{before} -> {after}"
    );
}

/// R 眩晕并伤害半径内的敌方英雄。
#[test]
fn erebos_r_stuns_and_damages_nearby_champion() {
    let mut app = build_app();
    insert_spell(&mut app, R_SPELL_KEY, "r_damage", 150.0);
    let caster = spawn_erebos(&mut app);
    let r = add_skill(&mut app, caster, SkillSlot::R, R_SPELL_KEY);

    let enemy = app
        .world_mut()
        .spawn((
            Champion,
            Team::Chaos,
            Transform::from_xyz(200.0, 0.0, 0.0),
            Health::new(2000.0),
        ))
        .id();

    cast(&mut app, caster, r, 3, Vec2::ZERO);
    for _ in 0..3 {
        app.update();
    }

    assert!(
        has_buff::<DebuffStun>(&app, enemy),
        "R 应眩晕半径内的敌方英雄 (DebuffStun)"
    );
    let hp = app.world().get::<Health>(enemy).unwrap().value;
    assert!(
        hp < 2000.0,
        "R 应对范围内敌方英雄造成魔法伤害，实际 {hp}"
    );
}

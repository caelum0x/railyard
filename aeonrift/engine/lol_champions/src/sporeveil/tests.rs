#![cfg(test)]
//! Headless, asset-free gate for Sporeveil (in-memory `Spell`, zero Riot assets).

use std::collections::BTreeMap;
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use lol_base::hash_key::HashKey;
use lol_base::spell::Spell;
use lol_base::spell::{DataSpell, ValuesEffect};
use lol_base::spell_calc::{
    CalculationPart, CalculationPartEffectValue, CalculationSpell, CalculationType,
};
use lol_core::action::PluginAction;
use lol_core::base::buff::Buffs;
use lol_core::buffs::cc_debuffs::{DebuffSlow, PluginCc};
use lol_core::buffs::common_buffs::PluginCommonBuffs;
use lol_core::cooldown::PluginCooldown;
use lol_core::damage::PluginDamage;
use lol_core::life::{Health, PluginLife};
use lol_core::movement::Movement;
use lol_core::skill::{CoolDown, EventSkillCast, PluginSkill, Skill, SkillOf, SkillSlot, Skills};
use lol_core::team::Team;

use crate::sporeveil::{Sporeveil, PluginSporeveil};

const TEST_FPS: f64 = 30.0;
const EPSILON: f32 = 1e-4;

fn spell_handle(key: u32) -> Handle<Spell> {
    Handle::from(HashKey::<Spell>::from(key))
}

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
    app.add_plugins(PluginSporeveil);
    app.init_asset::<Spell>();
    app.insert_resource(Time::<Fixed>::from_hz(TEST_FPS));
    app.insert_resource(TimeUpdateStrategy::FixedTimesteps(1));
    app
}

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

fn spawn_champ(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Sporeveil,
            Team::Order,
            Transform::default(),
            Skills::default(),
            Movement { speed: 335.0 },
            Health::new(1000.0),
        ))
        .id()
}

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
    if let Some(mut skills) = app.world_mut().get_mut::<Skills>(caster) {
        if !skills.iter().any(|e| e == skill_entity) {
            skills.push(skill_entity);
        }
    }
    skill_entity
}

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

fn spawn_enemy(app: &mut App, pos: Vec3, hp: f32) -> Entity {
    app.world_mut()
        .spawn((Team::Chaos, Transform::from_translation(pos), Health::new(hp)))
        .id()
}

#[test]
fn sporeveil_q_deals_damage() {
    let mut app = build_app();
    insert_spell(&mut app, 0xAE01A001, "q_damage", 175.0);
    let caster = spawn_champ(&mut app);
    let s = add_skill(&mut app, caster, SkillSlot::Q, 0xAE01A001);
    let enemy = spawn_enemy(&mut app, Vec3::new(0.0, 0.0, -300.0), 3000.0);
    cast(&mut app, caster, s, 0, Vec2::ZERO);
    for _ in 0..4 {
        app.update();
    }
    let hp = app.world().get::<Health>(enemy).unwrap().value;
    assert!((hp - 2825.0).abs() < EPSILON, "expected 3000 - 175.0 = 2825.0, got {hp}");
}

#[test]
fn sporeveil_q_is_deterministic() {
    fn run_once() -> f32 {
        let mut app = build_app();
        insert_spell(&mut app, 0xAE01A001, "q_damage", 175.0);
        let caster = spawn_champ(&mut app);
        let s = add_skill(&mut app, caster, SkillSlot::Q, 0xAE01A001);
        let enemy = spawn_enemy(&mut app, Vec3::new(0.0, 0.0, -300.0), 3000.0);
        cast(&mut app, caster, s, 0, Vec2::ZERO);
        for _ in 0..4 {
            app.update();
        }
        app.world().get::<Health>(enemy).unwrap().value
    }
    let a = run_once();
    let b = run_once();
    assert_eq!(a.to_bits(), b.to_bits(), "same input => bit-identical: {a} vs {b}");
    assert!((a - 2825.0).abs() < EPSILON, "expected 2825.0, got {a}");
}

#[test]
fn sporeveil_w_slows() {
    let mut app = build_app();
    insert_spell(&mut app, 0xAE01A002, "w_damage", 50.0);
    let caster = spawn_champ(&mut app);
    let s = add_skill(&mut app, caster, SkillSlot::W, 0xAE01A002);
    let enemy = spawn_enemy(&mut app, Vec3::new(0.0, 0.0, -270.0), 5000.0);
    cast(&mut app, caster, s, 1, Vec2::ZERO);
    for _ in 0..4 {
        app.update();
    }
    assert!(has_buff::<DebuffSlow>(&app, enemy), "W should apply DebuffSlow");
}


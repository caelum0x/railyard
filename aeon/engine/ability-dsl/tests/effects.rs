//! M5 INTERPRETER EFFECT TESTS.
//!
//! Verifies each targeting mode compiles DOWN to the right sim primitives:
//!   * `SelfCast` heal restores the caster and clamps at max HP,
//!   * `Target` dash+strike moves the caster AND damages + stuns the victim,
//!   * `Area` damage hits every enemy within radius (and no one outside it),
//!   * base-stat compilation maps attack_speed -> integer tick period.

use champions::spec::{AbilitySpec, BaseStats, ChampionSpec, Effect, Targeting};
use champions::{compile, execute_ability};
use sim::{Vec2, World};

fn champ_spec(name: &str, hp: i32, ad: i32, armor: i32) -> ChampionSpec {
    ChampionSpec {
        name: name.into(),
        base: BaseStats {
            hp,
            ad,
            armor,
            attack_speed: 1.0,
            move_speed: 10.0,
            range: 6.0,
        },
        abilities: vec![],
    }
}

#[test]
fn compile_maps_attack_speed_to_integer_period() {
    let mut spec = champ_spec("A", 100, 10, 0);
    spec.base.attack_speed = 2.0; // 2 attacks/sec at 30 Hz -> 15-tick period
    let champ = compile(&spec, 0, 0, Vec2::ZERO);
    assert_eq!(champ.attack_period, 15);
    assert_eq!(champ.health.max, 100);
    assert_eq!(champ.attack_damage, 10);
}

#[test]
fn self_cast_heal_restores_caster_and_clamps_at_max() {
    let spec = champ_spec("Healer", 200, 10, 0);
    let mut champ = compile(&spec, 0, 0, Vec2::ZERO);
    // Wound the champion to 50 HP before healing.
    champ.health.current = 50;
    let mut world = World::from_champions(1, vec![champ]);

    let heal = AbilitySpec {
        name: "Mend".into(),
        cooldown: 30,
        cost: None,
        targeting: Targeting::SelfCast,
        range: 0.0,
        effects: vec![Effect::Heal { amount: 1000 }], // huge, must clamp
    };
    execute_ability(&mut world, 0, &heal, Vec2::ZERO, None);

    assert_eq!(
        world.champion(0).unwrap().health.current,
        200,
        "heal must clamp at max HP, never overheal"
    );
}

#[test]
fn target_dash_strike_moves_caster_and_damages_and_stuns() {
    let caster = compile(
        &champ_spec("Bruiser", 500, 50, 0),
        0,
        0,
        Vec2::new(0.0, 0.0),
    );
    let victim = compile(&champ_spec("Dummy", 500, 0, 0), 1, 1, Vec2::new(10.0, 0.0));
    let mut world = World::from_champions(2, vec![caster, victim]);

    let charge = AbilitySpec {
        name: "Charge".into(),
        cooldown: 60,
        cost: None,
        targeting: Targeting::Target,
        range: 12.0,
        effects: vec![
            Effect::Dash { distance: 8.0 },
            Effect::Damage {
                amount: 60,
                scaling: None,
            },
            Effect::Stun { duration: 10 },
        ],
    };
    let victim_pos = Vec2::new(10.0, 0.0);
    execute_ability(&mut world, 0, &charge, victim_pos, Some(1));

    // Caster dashed 8 units toward the victim (from x=0 to x=8).
    let caster = world.champion(0).unwrap();
    assert!(
        (caster.pos.x - 8.0).abs() < 1e-4,
        "caster should have dashed to x=8"
    );
    // Victim took 60 damage (armor 0) and is stunned.
    let victim = world.champion(1).unwrap();
    assert_eq!(
        victim.health.current, 440,
        "victim should take exactly 60 damage"
    );
    assert_eq!(
        victim.stun_ticks, 10,
        "victim should be stunned for 10 ticks"
    );
}

#[test]
fn area_damage_hits_every_enemy_in_radius_only() {
    // Two enemies near the blast point, one far outside it.
    let caster = compile(&champ_spec("Bomber", 400, 40, 0), 0, 0, Vec2::new(0.0, 0.0));
    let near_a = compile(&champ_spec("NearA", 300, 0, 0), 1, 1, Vec2::new(10.0, 0.0));
    let near_b = compile(&champ_spec("NearB", 300, 0, 0), 2, 1, Vec2::new(11.0, 1.0));
    let far = compile(&champ_spec("Far", 300, 0, 0), 3, 1, Vec2::new(40.0, 0.0));
    let mut world = World::from_champions(3, vec![caster, near_a, near_b, far]);

    let nova = AbilitySpec {
        name: "Nova".into(),
        cooldown: 100,
        cost: None,
        targeting: Targeting::Area,
        range: 4.0, // radius around the aim point
        effects: vec![Effect::Damage {
            amount: 50,
            scaling: None,
        }],
    };
    let blast = Vec2::new(10.0, 0.0);
    execute_ability(&mut world, 0, &nova, blast, None);

    assert_eq!(world.champion(1).unwrap().health.current, 250, "near_a hit");
    assert_eq!(world.champion(2).unwrap().health.current, 250, "near_b hit");
    assert_eq!(
        world.champion(3).unwrap().health.current,
        300,
        "far enemy untouched"
    );
}

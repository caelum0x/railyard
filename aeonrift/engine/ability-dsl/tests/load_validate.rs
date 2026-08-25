//! M5 LOAD + VALIDATE TESTS.
//!
//! Covers the single gate that kills malformed champions:
//!   * every shipped example champion LOADS and VALIDATES,
//!   * a spec naming an effect/targeting OUTSIDE the closed vocabulary is REJECTED
//!     with a clear error (this is the structural, parse-time layer),
//!   * semantically broken specs (bad stats, empty effects, zero cooldown) are
//!     rejected by the validator with a precise message.

use champions::{load_file, load_str, SpecError};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn spec_path(name: &str) -> String {
    format!("{SPECS_DIR}/{name}")
}

// --- every shipped example champion loads + validates -----------------------

#[test]
fn example_sentinel_loads_and_validates() {
    let spec = load_file(spec_path("sentinel.ron")).expect("sentinel must load + validate");
    assert_eq!(spec.name, "Sentinel");
    assert_eq!(spec.abilities.len(), 1);
}

#[test]
fn example_pyromancer_loads_and_validates() {
    let spec = load_file(spec_path("pyromancer.ron")).expect("pyromancer must load + validate");
    assert_eq!(spec.name, "Pyromancer");
    assert_eq!(spec.abilities[0].name, "Fireball");
}

#[test]
fn example_vanguard_loads_and_validates() {
    let spec = load_file(spec_path("vanguard.ron")).expect("vanguard must load + validate");
    assert_eq!(spec.name, "Vanguard");
    assert_eq!(spec.abilities.len(), 2);
}

// --- closed vocabulary: unknown effect / targeting rejected at parse time ----

#[test]
fn unknown_effect_is_rejected_with_clear_error() {
    let ron = r#"ChampionSpec(
        name: "Bad",
        base: BaseStats(hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0),
        abilities: [ AbilitySpec(name: "X", cooldown: 10, targeting: Target, range: 5.0,
            effects: [ Poison(amount: 5) ]) ],
    )"#;
    let err = load_str(ron).expect_err("unknown effect must be rejected");
    match err {
        SpecError::Parse(msg) => {
            assert!(
                msg.contains("Poison"),
                "error should name the bad variant: {msg}"
            );
            assert!(
                msg.contains("Damage") && msg.contains("Stun"),
                "error should list the allowed effect vocabulary: {msg}"
            );
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn unknown_targeting_is_rejected_with_clear_error() {
    let ron = r#"ChampionSpec(
        name: "Bad",
        base: BaseStats(hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0),
        abilities: [ AbilitySpec(name: "X", cooldown: 10, targeting: Beam, range: 5.0,
            effects: [ Damage(amount: 5) ]) ],
    )"#;
    let err = load_str(ron).expect_err("unknown targeting must be rejected");
    let SpecError::Parse(msg) = err else {
        panic!("expected a parse error, got {err:?}");
    };
    assert!(
        msg.contains("Beam"),
        "error should name the bad targeting: {msg}"
    );
    assert!(
        msg.contains("Skillshot"),
        "error should list the allowed targeting vocabulary: {msg}"
    );
}

#[test]
fn unknown_field_is_rejected() {
    // `deny_unknown_fields` guards against typo'd / injected fields.
    let ron = r#"ChampionSpec(
        name: "Bad",
        base: BaseStats(hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0),
        abilities: [],
        secret_backdoor: true,
    )"#;
    assert!(matches!(load_str(ron), Err(SpecError::Parse(_))));
}

// --- semantic validation: precise, typed rejections -------------------------

fn base(stats: &str, abilities: &str) -> String {
    format!("ChampionSpec(name: \"T\", base: BaseStats({stats}), abilities: [{abilities}])")
}

#[test]
fn non_positive_hp_is_rejected() {
    let ron = base(
        "hp: 0, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0",
        "",
    );
    assert!(matches!(load_str(&ron), Err(SpecError::Stat(_))));
}

#[test]
fn non_positive_attack_speed_is_rejected() {
    let ron = base(
        "hp: 100, ad: 10, armor: 0, attack_speed: 0.0, move_speed: 10.0, range: 5.0",
        "",
    );
    assert!(matches!(load_str(&ron), Err(SpecError::Stat(_))));
}

#[test]
fn zero_cooldown_ability_is_rejected() {
    let ability = r#"AbilitySpec(name: "Z", cooldown: 0, targeting: SelfCast, effects: [ Heal(amount: 10) ])"#;
    let ron = base(
        "hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0",
        ability,
    );
    assert!(matches!(load_str(&ron), Err(SpecError::Ability { .. })));
}

#[test]
fn empty_effects_ability_is_rejected() {
    let ability = r#"AbilitySpec(name: "Z", cooldown: 10, targeting: SelfCast, effects: [])"#;
    let ron = base(
        "hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0",
        ability,
    );
    let err = load_str(&ron).expect_err("empty effects must be rejected");
    assert!(matches!(err, SpecError::Ability { .. }), "got {err:?}");
}

#[test]
fn ranged_ability_without_range_is_rejected() {
    // A Skillshot with range 0 is meaningless and must be rejected.
    let ability = r#"AbilitySpec(name: "Z", cooldown: 10, targeting: Skillshot, range: 0.0,
        effects: [ Damage(amount: 10) ])"#;
    let ron = base(
        "hp: 100, ad: 10, armor: 0, attack_speed: 1.0, move_speed: 10.0, range: 5.0",
        ability,
    );
    assert!(matches!(load_str(&ron), Err(SpecError::Ability { .. })));
}

//! Action-based input for the MOBA client, via `leafwing-input-manager` (Bevy 0.19).
//!
//! This is the client VIEW layer: it turns raw keyboard/mouse/touch into the game's
//! high-level [`MobaAction`] verbs, reads them as an `ActionState`, and emits
//! deterministic [`sim::Input`] intents into [`PendingIntents`] for the netcode /
//! prediction driver to consume. The authoritative sim (`sim::step`) is untouched and
//! remains the source of truth; nothing here decides an ability's real outcome.
//!
//! ## Input mapping (desktop keyboard+mouse; touch on mobile)
//! | Action    | Desktop bind        | Meaning (client intent)                     |
//! |-----------|---------------------|---------------------------------------------|
//! | `Move`    | Right mouse button  | click-to-move toward the cursor world point |
//! | `CastQ/W/E/R` | Q / W / E / R   | skillshot aimed at the cursor (CD/mana gated)|
//! | `Recall`  | B                   | UX signal (begin recall) — non-sim          |
//! | `Shop`    | P                   | UX signal (toggle shop) — non-sim           |
//! | `Ping`    | G                   | UX signal (ping cursor) — non-sim           |
//!
//! Positional intent (the move destination, the cast aim) is read from the pointer —
//! `leafwing-input-manager` maps the *verb*, while the cursor/touch world position
//! supplies the *where*; the two are combined here. Touch taps feed the same
//! [`CursorWorld`] as the mouse, so an on-screen ability button on mobile dispatches
//! exactly the same `MobaAction`.

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

use sim::{Command, EntityId, Input, Vec2};

use crate::clientability::{tick_abilities, AbilityBook, AbilitySlot};

/// The MOBA's high-level input verbs. All buttonlike; the *where* of a move/cast
/// comes from the pointer (see [`CursorWorld`]).
#[derive(Actionlike, PartialEq, Eq, Hash, Clone, Copy, Debug, Reflect)]
pub enum MobaAction {
    /// Click-to-move toward the cursor.
    Move,
    /// Cast the Q / W / E / R ability toward the cursor.
    CastQ,
    CastW,
    CastE,
    CastR,
    /// Begin recall (client UX only).
    Recall,
    /// Toggle the item shop (client UX only).
    Shop,
    /// Ping the cursor location (client UX only).
    Ping,
}

impl MobaAction {
    /// The ability slot a cast action drives, if any (`Move`/`Recall`/`Shop`/`Ping`
    /// are not casts).
    pub fn ability_slot(self) -> Option<AbilitySlot> {
        match self {
            MobaAction::CastQ => Some(AbilitySlot::Q),
            MobaAction::CastW => Some(AbilitySlot::W),
            MobaAction::CastE => Some(AbilitySlot::E),
            MobaAction::CastR => Some(AbilitySlot::R),
            _ => None,
        }
    }
}

/// The default desktop keyboard+mouse binding. Right-click moves (LoL-style),
/// Q/W/E/R cast, and B/P/G drive the recall/shop/ping UX verbs.
pub fn default_input_map() -> InputMap<MobaAction> {
    InputMap::default()
        .with(MobaAction::Move, MouseButton::Right)
        .with(MobaAction::CastQ, KeyCode::KeyQ)
        .with(MobaAction::CastW, KeyCode::KeyW)
        .with(MobaAction::CastE, KeyCode::KeyE)
        .with(MobaAction::CastR, KeyCode::KeyR)
        .with(MobaAction::Recall, KeyCode::KeyB)
        .with(MobaAction::Shop, KeyCode::KeyP)
        .with(MobaAction::Ping, KeyCode::KeyG)
}

/// The `sim` entity id of the champion this client controls. Set once the local
/// champion is known (spawned / assigned by the server).
#[derive(Resource, Clone, Copy, Debug)]
pub struct LocalChampion(pub EntityId);

/// The latest pointer position in sim-world space (updated uniformly from mouse and
/// touch). Move/cast intents aim here. (`sim::Vec2` has no `Default`, so it is given
/// explicitly as the origin.)
#[derive(Resource, Clone, Copy, Debug)]
pub struct CursorWorld(pub Vec2);

impl Default for CursorWorld {
    fn default() -> Self {
        CursorWorld(Vec2::ZERO)
    }
}

/// Deterministic intents produced this frame by the local player, awaiting handoff to
/// the sim / netcode driver (drained each tick). Keeping them in a plain `Vec`
/// preserves issue order and stays engine-neutral — the sim never sees Bevy types.
#[derive(Resource, Clone, Debug, Default)]
pub struct PendingIntents(pub Vec<Input>);

/// Non-sim client UX verbs raised this frame (recall/shop/ping), for the HUD to act
/// on. Kept separate from [`PendingIntents`] because they never touch the sim.
#[derive(Resource, Clone, Debug, Default)]
pub struct UxSignals {
    pub recall: bool,
    pub shop_toggled: bool,
    pub ping: Option<Vec2>,
}

/// Wires the action-input VIEW into a client `App`: registers the input plugin,
/// spawns the local controller (holding the [`InputMap`]), installs the ability
/// book + intent/UX resources, and runs the per-frame read systems.
///
/// Compose it onto a Bevy app that also provides `DefaultPlugins` (for a real window
/// + pointer). Callers set [`LocalChampion`] once their champion id is known and
/// drain [`PendingIntents`] each tick into `sim::step` (or the netcode input buffer).
#[derive(Default)]
pub struct MobaInputPlugin;

impl Plugin for MobaInputPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InputManagerPlugin::<MobaAction>::default())
            .init_resource::<CursorWorld>()
            .init_resource::<PendingIntents>()
            .init_resource::<UxSignals>()
            .init_resource::<AbilityBook>()
            .add_systems(Startup, spawn_local_controller)
            .add_systems(
                Update,
                (update_cursor_world, tick_abilities, emit_intents).chain(),
            );
    }
}

/// Spawn the entity that carries the local player's [`InputMap`] (+ its `ActionState`).
fn spawn_local_controller(mut commands: Commands) {
    commands.spawn((
        default_input_map(),
        ActionState::<MobaAction>::default(),
        Name::new("MobaLocalController"),
    ));
}

/// Update [`CursorWorld`] from the mouse cursor, falling back to the most recent
/// touch. No-ops when there is no window/camera (e.g. a headless smoke app).
fn update_cursor_world(
    windows: Query<&Window>,
    touches: Option<Res<Touches>>,
    camera: Query<(&Camera, &GlobalTransform)>,
    mut cursor: ResMut<CursorWorld>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };
    // Prefer the mouse cursor; otherwise take the first active touch. `Touches` is
    // optional so this system stays panic-free in a headless app without InputPlugin.
    let screen = window.cursor_position().or_else(|| {
        touches
            .as_ref()
            .and_then(|t| t.iter().next().map(|touch| touch.position()))
    });
    if let Some(screen) = screen {
        // `viewport_to_world_2d` yields a `bevy::Vec2`; convert to the sim's `Vec2`
        // (the two are distinct types) so intents stay engine-neutral.
        if let Ok(world) = camera.viewport_to_world_2d(cam_tf, screen) {
            cursor.0 = Vec2::new(world.x, world.y);
        }
    }
}

/// Read the local `ActionState` and emit this frame's intents:
/// - `Move` (held or just pressed) → a `MoveTo` toward the cursor,
/// - `CastQ/W/E/R` (just pressed) → a cooldown/mana-gated `CastSkillshot`,
/// - `Recall/Shop/Ping` (just pressed) → non-sim [`UxSignals`].
fn emit_intents(
    controller: Query<&ActionState<MobaAction>>,
    local: Option<Res<LocalChampion>>,
    cursor: Res<CursorWorld>,
    mut book: ResMut<AbilityBook>,
    mut pending: ResMut<PendingIntents>,
    mut ux: ResMut<UxSignals>,
) {
    let Ok(state) = controller.single() else {
        return;
    };
    let aim = cursor.0;

    // UX verbs first (independent of whether we control a champion yet).
    ux.recall = state.just_pressed(&MobaAction::Recall);
    ux.shop_toggled = state.just_pressed(&MobaAction::Shop);
    ux.ping = state
        .just_pressed(&MobaAction::Ping)
        .then_some(aim);

    // Sim intents require a controlled champion.
    let Some(local) = local else {
        return;
    };
    let entity = local.0;

    // Click-to-move: honor a held right mouse button, not just the click edge.
    if state.pressed(&MobaAction::Move) {
        pending.0.push(Input {
            entity,
            command: Command::MoveTo(aim),
        });
    }

    // Casts: only on the press edge, and only if the client CD + mana allow it.
    for action in [
        MobaAction::CastQ,
        MobaAction::CastW,
        MobaAction::CastE,
        MobaAction::CastR,
    ] {
        if state.just_pressed(&action) {
            if let Some(slot) = action.ability_slot() {
                if book.try_cast(slot) {
                    pending.0.push(Input {
                        entity,
                        command: Command::CastSkillshot(aim),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_actions_map_to_slots() {
        assert_eq!(MobaAction::CastQ.ability_slot(), Some(AbilitySlot::Q));
        assert_eq!(MobaAction::CastR.ability_slot(), Some(AbilitySlot::R));
        assert_eq!(MobaAction::Move.ability_slot(), None);
        assert_eq!(MobaAction::Shop.ability_slot(), None);
    }

    #[test]
    fn input_map_binds_every_action() {
        let map = default_input_map();
        // Every MobaAction variant has at least one bound input.
        for action in [
            MobaAction::Move,
            MobaAction::CastQ,
            MobaAction::CastW,
            MobaAction::CastE,
            MobaAction::CastR,
            MobaAction::Recall,
            MobaAction::Shop,
            MobaAction::Ping,
        ] {
            assert!(
                map.get_buttonlike(&action).map_or(false, |b| !b.is_empty()),
                "action {action:?} should have a binding"
            );
        }
    }

    #[test]
    fn plugin_builds_into_minimal_app() {
        // The VIEW layer must compose onto a headless app without panicking.
        // InputPlugin supplies the ButtonInput/Touches resources leafwing-input-manager
        // reads; MinimalPlugins supplies Time for the ability tick.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(bevy::input::InputPlugin)
            .add_plugins(MobaInputPlugin);
        app.insert_resource(LocalChampion(7));
        app.update();
        // Startup spawned exactly one controller carrying the InputMap.
        let count = app
            .world_mut()
            .query::<&InputMap<MobaAction>>()
            .iter(app.world())
            .count();
        assert_eq!(count, 1);
    }
}

//! # Aeonrift: Ascendancy — the mobile client (a Bevy VIEW over the deterministic core)
//!
//! This crate renders and drives a match of the `cards` (`aeonrift-ascendancy`) rules
//! core. It is deliberately THIN: it holds no rules of its own. Match state — lanes,
//! elixir, bases, deployed units, the win condition — lives entirely in
//! `crate::MatchState`, which drives the shared `sim` combat core. The client only:
//!
//! 1. renders that state each frame (bases, units, elixir, hand) — see [`render`] +
//!    [`hud`],
//! 2. feeds player intent back in by calling `crate::try_deploy` — see [`input`],
//! 3. advances the match on a FIXED timestep via `crate::step_tick` — see [`tick`].
//!
//! ## Cross-platform entry point
//! [`main`] is annotated with `#[bevy_main]`, which (with the `cdylib` crate type in
//! `Cargo.toml`) provides the Android/iOS entry point per Bevy's `mobile` example. The
//! desktop binary (`src/main.rs`) calls the same function. Mobile build commands live
//! in `README.md`; on-device builds need Xcode / the Android SDK.
//!
//! A `--headless` mode (or `AEONRIFT_HEADLESS=1`) runs the same app with
//! `MinimalPlugins`, stepping the match without a window and printing the result —
//! used to smoke-test the client-over-core seam where no display is available.

pub mod actions;
pub mod game;
pub mod hud;
pub mod input;
pub mod render;
pub mod screens;
pub mod tick;

use bevy::app::AppExit;
use bevy::prelude::*;

use crate::match_hash;
use actions::CardInputPlugin;
use game::{new_battle, Battle, RunOptions, Selection, StatusMessage};
use tick::{step_once, TICK_HZ};

/// Resolve run options from CLI args / environment.
fn resolve_options() -> RunOptions {
    let args: Vec<String> = std::env::args().collect();
    let headless =
        args.iter().any(|a| a == "--headless") || std::env::var("AEONRIFT_HEADLESS").is_ok();
    let headless_max_ticks = args
        .iter()
        .position(|a| a == "--ticks")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    RunOptions {
        headless,
        headless_max_ticks,
    }
}

/// The cross-platform entry point. `#[bevy_main]` generates the Android `android_main`
/// wrapper; `main.rs` calls this directly on desktop.
#[bevy_main]
pub fn main() {
    let options = resolve_options();
    if options.headless {
        run_headless(options);
    } else {
        run_windowed(options);
    }
}

/// Build + run the full windowed/touch app.
fn run_windowed(options: RunOptions) {
    App::new()
        .add_plugins(
            DefaultPlugins
                // Point Bevy's asset root at the repo `assets/` dir so the hand can load
                // the real finished card faces under `cards/final-v2/`.
                .set(AssetPlugin {
                    file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets").to_string(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Aeonrift: Ascendancy".into(),
                        resolution: (450u32, 820u32).into(),
                        // iOS-only niceties (harmless elsewhere), matching Bevy's mobile example.
                        recognize_rotation_gesture: true,
                        prefers_home_indicator_hidden: true,
                        prefers_status_bar_hidden: true,
                        ..default()
                    }),
                    ..default()
                }),
        )
        // Fixed timestep == the core's tick rate: the sim is stepped deterministically,
        // never coupled to the render frame rate.
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
        .insert_resource(options)
        .init_resource::<Selection>()
        .init_resource::<StatusMessage>()
        // Action-based input VIEW (leafwing-input-manager): keyboard/touch card
        // verbs alongside the existing pointer flow. Deterministic core untouched.
        .add_plugins(CardInputPlugin)
        .add_systems(Startup, hud::setup)
        .add_systems(FixedUpdate, tick::advance_match)
        .add_systems(
            Update,
            (
                input::handle_input,
                render::draw_battle,
                hud::update_hand,
                hud::update_elixir,
                hud::update_bases,
                hud::update_message,
                hud::update_winner,
            ),
        )
        .run();
}

/// Headless smoke: step the match with `MinimalPlugins` (no window/GPU) until it
/// resolves or the tick cap is hit, print the result, and exit. Proves the Bevy app
/// links + drives the deterministic core in an environment without a display.
fn run_headless(options: RunOptions) {
    App::new()
        .add_plugins(MinimalPlugins)
        .insert_resource(options)
        .insert_resource(new_battle())
        .add_systems(Update, headless_step)
        .run();
}

/// One core tick per app update; reports and exits when the match ends or caps out.
fn headless_step(
    mut battle: ResMut<Battle>,
    options: Res<RunOptions>,
    mut exit: MessageWriter<AppExit>,
) {
    step_once(&mut battle);

    let over = battle.state.is_over();
    let capped = battle.state.world.tick >= options.headless_max_ticks;
    if over || capped {
        println!("Aeonrift: Ascendancy — headless client smoke (Bevy MinimalPlugins)");
        match battle.state.outcome {
            Some(crate::Outcome::Winner(p)) => {
                let who = if p == game::HUMAN { "human" } else { "bot" };
                println!("WINNER: player {p} ({who}) destroyed the enemy base");
            }
            Some(crate::Outcome::Draw) => println!("RESULT: DRAW"),
            None => println!("RESULT: undecided at tick cap"),
        }
        println!(
            "base HP: player 0 = {}   player 1 = {}",
            battle.state.base_hp(0),
            battle.state.base_hp(1)
        );
        println!(
            "units deployed: player 0 = {}   player 1 = {}",
            battle.state.deployed_count[0], battle.state.deployed_count[1]
        );
        println!(
            "ticks={} match_hash={}",
            battle.state.world.tick,
            match_hash(&battle.state)
        );
        exit.write(AppExit::Success);
    }
}

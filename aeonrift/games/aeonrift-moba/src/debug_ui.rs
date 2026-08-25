//! Client-side debug / HUD overlay built on `bevy_egui` (Bevy 0.19).
//!
//! This is a **presentational, non-authoritative** layer behind the `client`
//! feature. It draws immediate-mode egui panels showing match telemetry (tick,
//! phase, per-team gold, entity counts) that the client mirrors from authoritative
//! snapshots — it never mutates the deterministic sim.
//!
//! ## Wiring
//! Add [`DebugUiPlugin`] to a Bevy client `App`. Each frame the client copies the
//! latest known match telemetry into the [`HudState`] resource; the egui panel is
//! rendered from that resource inside bevy_egui's [`EguiPrimaryContextPass`], the
//! schedule bevy_egui runs UI systems in for the primary (window) context.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use aeonrift_moba::debug_ui::{DebugUiPlugin, HudState};
//!
//! let mut app = App::new();
//! app.add_plugins(DefaultPlugins);
//! app.add_plugins(DebugUiPlugin);
//! // Each frame, feed authoritative telemetry into the HUD resource:
//! app.add_systems(Update, |mut hud: ResMut<HudState>| {
//!     hud.tick += 1;
//! });
//! ```

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin, EguiPrimaryContextPass};

/// Telemetry the HUD renders. Populated by the client from authoritative snapshots;
/// the panel only ever reads it, so it can never affect the deterministic sim.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct HudState {
    /// Current authoritative tick the client has reconciled to.
    pub tick: u64,
    /// Human-readable match phase (e.g. "InGame", "Ended: BLUE").
    pub phase: String,
    /// Per-team gold (blue, red).
    pub gold: (i64, i64),
    /// Live champion / minion / structure counts for a quick sanity read.
    pub champions: u32,
    pub minions: u32,
    pub structures: u32,
    /// Whether the overlay is visible (toggle with the `visible` field).
    pub visible: bool,
}

impl Default for HudState {
    fn default() -> Self {
        HudState {
            tick: 0,
            phase: "Loading".to_string(),
            gold: (0, 0),
            champions: 0,
            minions: 0,
            structures: 0,
            visible: true,
        }
    }
}

/// Adds the egui debug/HUD overlay to a Bevy client app.
pub struct DebugUiPlugin;

impl Plugin for DebugUiPlugin {
    fn build(&self, app: &mut App) {
        // EguiPlugin is idempotent-safe to add once; guard so stacking client
        // plugins that also want egui don't double-register it.
        if !app.is_plugin_added::<EguiPlugin>() {
            app.add_plugins(EguiPlugin::default());
        }
        app.init_resource::<HudState>();
        // UI systems for the primary (window) context must run in this schedule.
        app.add_systems(EguiPrimaryContextPass, hud_panel);
    }
}

/// Render the debug HUD from [`HudState`]. Runs in bevy_egui's primary-context
/// pass. If the primary egui context isn't ready yet (e.g. before a window/camera
/// exists) it simply skips this frame.
fn hud_panel(mut contexts: EguiContexts, hud: Res<HudState>) {
    if !hud.visible {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    egui::Window::new("AEONRIFT — Debug HUD")
        .default_pos([12.0, 12.0])
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!("tick: {}", hud.tick));
            ui.label(format!("phase: {}", hud.phase));
            ui.separator();
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::LIGHT_BLUE, format!("BLUE gold: {}", hud.gold.0));
                ui.separator();
                ui.colored_label(egui::Color32::LIGHT_RED, format!("RED gold: {}", hud.gold.1));
            });
            ui.separator();
            egui::Grid::new("entity-counts").show(ui, |ui| {
                ui.label("champions");
                ui.label(hud.champions.to_string());
                ui.end_row();
                ui.label("minions");
                ui.label(hud.minions.to_string());
                ui.end_row();
                ui.label("structures");
                ui.label(hud.structures.to_string());
                ui.end_row();
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_state_default_is_visible_and_zeroed() {
        let hud = HudState::default();
        assert!(hud.visible);
        assert_eq!(hud.tick, 0);
        assert_eq!(hud.gold, (0, 0));
    }

    #[test]
    fn hud_state_toggles_and_updates() {
        // The HUD resource is plain data the client mirrors from snapshots; verify
        // it round-trips without touching the sim or a render context.
        let mut hud = HudState::default();
        hud.tick = 128;
        hud.phase = "Ended: BLUE".into();
        hud.gold = (5100, 4800);
        hud.champions = 10;
        hud.minions = 24;
        hud.structures = 22;
        hud.visible = false;
        assert_eq!(hud.tick, 128);
        assert!(!hud.visible);
        assert_eq!(hud.gold, (5100, 4800));
    }

    // NOTE: a full `App::add_plugins(DebugUiPlugin)` test is intentionally omitted:
    // `EguiPlugin` requires the Bevy render/asset context (`Assets<Shader>`), which
    // only exists once `DefaultPlugins` is added on a real client with a GPU/window.
    // That is expected engine behavior, not something this crate should stub out;
    // the plugin is exercised by the `--features client` build compiling it.
}

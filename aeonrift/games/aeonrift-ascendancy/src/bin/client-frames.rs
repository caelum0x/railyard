//! `client-frames` — headless OFFSCREEN render of the polished Bevy client to PNGs.
//!
//! This is the "seeing" proof for the client polish with NO window and NO screen
//! capture: it builds a real Bevy render app (full GPU pipeline via wgpu/Metal), points
//! a camera at an off-screen `Image` render target, drives a scripted board state
//! through the SAME client view systems the windowed game uses
//! (`client::render::sync_unit_sprites` for textured board units,
//! `client::vfx` for the ability-cast bursts, HDR + bloom), and reads each rendered
//! frame back to a PNG via Bevy's `Screenshot::image` + `save_to_disk`.
//!
//! It then writes `CLIENT-POLISH.html`, a contact sheet of the captured frames.
//!
//! Headless plumbing follows Bevy's own `headless_renderer` example: the winit windowing
//! plugin is disabled and a `ScheduleRunnerPlugin` drives the loop, so the render graph
//! runs to an image with no display attached.
//!
//! Run:  cargo run -p aeonrift-ascendancy --features client --bin client-frames

use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::asset::RenderAssetUsages;
use bevy::camera::{Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;

use cards::client::game::{
    face_asset_path, new_battle, slug_for_card, team_color, Battle, BASE_SIZE, CAMERA_Y, CARD_H,
    CARD_ROW_Y, CARD_W, DECK_SLUGS, HUMAN, VIEW_HEIGHT,
};
use cards::client::input::card_center_x;
use cards::client::tick::step_once;
use cards::client::{render, vfx};
use cards::Deploy;

/// Off-screen frame size (portrait, phone-like). FixedVertical keeps the same world
/// slice visible; only the horizontal field of view depends on the aspect.
const FRAME_W: u32 = 760;
const FRAME_H: u32 = 1320;

/// How many captured PNG frames the contact sheet holds.
const FRAMES: usize = 4;

/// Sim ticks advanced per rendered app frame during the capture phase.
const STEPS_PER_FRAME: u32 = 2;

/// App frames to let textures load + bursts build before the first capture.
const WARM_FRAMES: u64 = 40;
/// App frames between successive captures.
const CAPTURE_GAP: u64 = 16;

/// The off-screen render target image handle (a resource so systems can screenshot it).
#[derive(Resource, Clone)]
struct TargetImage(Handle<Image>);

/// Where the PNG frames are written (crate `replay/` dir, alongside the HTML gallery).
#[derive(Resource, Clone)]
struct OutDir(PathBuf);

/// Drives capture: advances the sim, requests screenshots on a schedule, then exits.
#[derive(Resource, Default)]
struct Director {
    frame: u64,
    captured: usize,
    next_capture: u64,
    finished_at: Option<u64>,
    paths: Vec<PathBuf>,
}

fn main() {
    let out_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/replay"));
    std::fs::create_dir_all(&out_dir).expect("create replay out dir");

    // A scripted, lively board: both sides field units that march to mid-field and cast.
    let mut battle = new_battle();
    warmup(&mut battle);

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets").to_string(),
                ..default()
            })
            // No windows: run fully headless.
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            })
            .disable::<WinitPlugin>(),
    )
    // Drive the app loop without winit (render graph still runs to the image target).
    .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
        1.0 / 60.0,
    )))
    .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
    .insert_resource(battle)
    // `draw_field` reads the hand `Selection` (none in this scripted view).
    .init_resource::<cards::client::game::Selection>()
    .init_resource::<vfx::CastTracker>()
    .insert_resource(OutDir(out_dir))
    .insert_resource(Director {
        next_capture: WARM_FRAMES,
        ..default()
    })
    .add_systems(Startup, setup_scene)
    .add_systems(
        Update,
        (
            director,
            render::sync_unit_sprites,
            vfx::spawn_cast_vfx,
            vfx::animate_vfx,
            render::draw_field,
            render::draw_unit_overlays,
        )
            .chain(),
    );

    app.run();

    // The contact sheet is written after the render app exits (all PNGs on disk).
    // Recover the frame list deterministically from the known naming scheme.
    let out_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/replay"));
    let frame_paths: Vec<PathBuf> = (0..FRAMES)
        .map(|i| out_dir.join(format!("client_frame_{i:02}.png")))
        .filter(|p| p.exists())
        .collect();
    let html = out_dir.join("CLIENT-POLISH.html");
    write_contact_sheet(&html, &frame_paths).expect("write CLIENT-POLISH.html");

    println!("Aeonrift: Ascendancy — headless client-polish offscreen render");
    println!("wrote {} PNG frame(s):", frame_paths.len());
    for p in &frame_paths {
        println!("  {}", p.display());
    }
    println!("  {}", html.display());
}

/// Deploy a spread of units for both sides and march them into engagement, so the
/// captured frames show a busy board with units casting abilities. Uses only the real
/// deploy + tick seams (nothing is faked into the sim).
fn warmup(battle: &mut Battle) {
    let lanes = battle.state.config.lanes;
    // Several waves: accumulate elixir, then deploy player 0 across the lanes. Player 1
    // is driven by the core bot inside `step_once`, so both sides field units.
    for wave in 0..10u32 {
        for _ in 0..55 {
            step_once(battle);
        }
        for lane in 0..lanes {
            let card = ((wave as usize) + lane) % DECK_SLUGS.len();
            if let Ok(id) = cards::try_deploy(&mut battle.state, Deploy::new(HUMAN, card, lane)) {
                if let Some(slug) = slug_for_card(card) {
                    battle.record_unit(id, slug);
                }
            }
        }
    }
    // March to first contact: stop as soon as skillshots are in flight on a full board.
    for _ in 0..600 {
        step_once(battle);
        let engaged = !battle.state.world.projectiles.is_empty();
        let crowded = battle.state.world.champions.len() >= 8;
        if engaged && crowded {
            break;
        }
    }
}

/// Spawn the off-screen camera (HDR + bloom), the two bases, and the hand-card row, so
/// the frame reads like the real client. Board units + VFX are added by the client
/// systems each frame.
fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    battle: Res<Battle>,
) {
    // Off-screen render target image.
    let size = Extent3d {
        width: FRAME_W,
        height: FRAME_H,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    let handle = images.add(image);
    commands.insert_resource(TargetImage(handle.clone()));

    // Camera → image target, with HDR + bloom so the VFX bursts glow (same look as the
    // windowed client's camera in `hud::setup`).
    commands.spawn((
        Camera2d,
        // In Bevy 0.19 the render target is its own component (required by `Camera`).
        RenderTarget::Image(handle.into()),
        Hdr,
        Tonemapping::TonyMcMapface,
        Bloom {
            intensity: 0.45,
            ..Bloom::default()
        },
        Projection::from(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: VIEW_HEIGHT,
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::from_xyz(0.0, CAMERA_Y, 0.0),
    ));

    // Bases (team-colored squares).
    for player in [0u8, 1u8] {
        let pos = battle.state.config.base_pos(player);
        commands.spawn((
            Sprite::from_color(team_color(player), Vec2::splat(BASE_SIZE)),
            Transform::from_xyz(pos.x, pos.y, 0.0),
        ));
    }

    // Hand row (real finished card faces), for HUD context at the bottom.
    let n = DECK_SLUGS.len();
    for (i, slug) in DECK_SLUGS.iter().enumerate() {
        let cx = card_center_x(i, n);
        commands.spawn((
            Sprite {
                image: asset_server.load(face_asset_path(slug)),
                custom_size: Some(Vec2::new(CARD_W, CARD_H)),
                ..default()
            },
            Transform::from_xyz(cx, CARD_ROW_Y, 0.5),
        ));
    }
}

/// Advance the scripted match and capture frames on a schedule, then exit.
fn director(
    mut commands: Commands,
    mut battle: ResMut<Battle>,
    mut director: ResMut<Director>,
    target: Res<TargetImage>,
    out: Res<OutDir>,
    mut exit: MessageWriter<AppExit>,
) {
    director.frame += 1;

    // Keep the battle live so casts (and thus VFX) keep happening across the captures.
    for _ in 0..STEPS_PER_FRAME {
        step_once(&mut battle);
    }
    // If the match ends, re-seed a fresh lively board so there is always something to show.
    if battle.state.is_over() {
        let mut fresh = new_battle();
        warmup(&mut fresh);
        *battle = fresh;
    }

    // Time to capture another frame?
    if director.captured < FRAMES && director.frame >= director.next_capture {
        // Prefer a frame that actually has a skillshot in flight (a live cast burst).
        let has_cast = !battle.state.world.projectiles.is_empty();
        if has_cast || director.frame >= director.next_capture + 30 {
            let path = out.0.join(format!("client_frame_{:02}.png", director.captured));
            commands
                .spawn(Screenshot(RenderTarget::Image(target.0.clone().into())))
                .observe(save_to_disk(path.clone()));
            director.paths.push(path);
            director.captured += 1;
            director.next_capture = director.frame + CAPTURE_GAP;
        }
    }

    // Once all frames are requested, run a flush margin so async readbacks finish, then exit.
    if director.captured >= FRAMES && director.finished_at.is_none() {
        director.finished_at = Some(director.frame + 30);
    }
    if let Some(done) = director.finished_at {
        if director.frame >= done {
            exit.write(AppExit::Success);
        }
    }
}

/// The frames live in the same directory as the HTML, so a bare filename is the src.
fn frame_src(to: &Path) -> String {
    to.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| to.to_string_lossy().into_owned())
}

/// Write a simple contact-sheet HTML that shows the captured frames side by side.
fn write_contact_sheet(path: &Path, frames: &[PathBuf]) -> std::io::Result<()> {
    let cells: String = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let src = frame_src(f);
            format!(
                r#"<figure><img src="{src}" alt="frame {i}"/><figcaption>Frame {n} — offscreen Bevy render</figcaption></figure>"#,
                n = i + 1
            )
        })
        .collect();
    let doc = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>Aeonrift: Ascendancy — Client Polish</title>
<style>
:root{{color-scheme:dark}}
body{{margin:0;background:#07060c;color:#ece7f6;font-family:system-ui,Segoe UI,sans-serif;
  padding:24px;display:flex;flex-direction:column;align-items:center;gap:8px}}
h1{{font-family:Georgia,serif;letter-spacing:4px;font-size:22px;margin:0}}
p.sub{{color:#9a8fbe;font-size:13px;letter-spacing:1px;margin:2px 0 18px;max-width:760px;text-align:center}}
.sheet{{display:flex;flex-wrap:wrap;gap:18px;justify-content:center}}
figure{{margin:0;background:linear-gradient(180deg,#140d22,#0a0712);border:1px solid #2a2140;
  border-radius:18px;padding:10px;box-shadow:0 18px 50px rgba(0,0,0,.55)}}
figure img{{width:300px;height:auto;display:block;border-radius:12px}}
figcaption{{margin-top:8px;font-size:11px;letter-spacing:1px;color:#b7a9e0;text-align:center}}
</style></head><body>
<h1>AEONRIFT · ASCENDANCY</h1>
<p class="sub">Client polish — real final-v2 champion faces on the board units, faction-tinted frames + HP bars,
and stylized ability-cast VFX bursts (HDR + bloom). Rendered fully headless: an off-screen Bevy render-to-image
camera, read back to PNG (no window, no screen capture).</p>
<div class="sheet">{cells}</div>
</body></html>"#
    );
    std::fs::write(path, doc)
}

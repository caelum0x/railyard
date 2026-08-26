//! Headless Bevy 0.19 cel/toon turntable renderer for the Anubis vertical slice.
//! - Loads the rigged+animated GLB (mesh + skeleton + Idle/Walk clips).
//! - Swaps StandardMaterial -> MtoonMaterial (banded toon diffuse + parametric fresnel rim).
//! - Adds a bevy_mod_outline silhouette (OutlineVolume).
//! - HDR camera + Bloom(OLD_SCHOOL) + Tonemapping, rendered to an offscreen Image.
//! - Deterministically seeks the animation and orbits a turntable, saving PNG frames.
//! Fully headless: no window, no winit, no screencapture. `Screenshot::image` -> `save_to_disk`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::asset::RenderAssetUsages;
use bevy::camera::{Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::image::Image;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::window::screenshot::{save_to_disk, Screenshot, ScreenshotCaptured};
use bevy::window::ExitCondition;

use bevy_mod_outline::{OutlinePlugin, OutlineVolume};
use bevy_shader_mtoon::{MtoonMaterial, MtoonPlugin, MtoonSun, VrmOutlineMode};

const SIZE: u32 = 1024;

#[derive(Resource, Clone)]
struct Cfg {
    out_dir: PathBuf,
    tag: String,
    clip: usize,
    frames: u32,
}

#[derive(Resource)]
struct GlbFile(String);

#[derive(Resource, Clone)]
struct Target(Handle<Image>);

#[derive(Resource)]
struct Anim {
    graph: Handle<AnimationGraph>,
    nodes: Vec<AnimationNodeIndex>,
    clips: Vec<Handle<AnimationClip>>,
}

#[derive(Resource, Default)]
struct Progress {
    mats_done: bool,
    player_done: bool,
    warm: u32,
    frame: u32,
    saved: Arc<AtomicU32>,
}

#[derive(Component)]
struct ModelRoot;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // args: <glb_abs> <out_dir> <clip:0|1> <frames> <tag>
    let glb = PathBuf::from(&args[1]);
    let out_dir = PathBuf::from(&args[2]);
    let clip: usize = args[3].parse().unwrap_or(1);
    let frames: u32 = args[4].parse().unwrap_or(36);
    let tag = args.get(5).cloned().unwrap_or_else(|| "walk".into());
    std::fs::create_dir_all(&out_dir).ok();

    let asset_dir = glb.parent().unwrap().to_path_buf();
    let file_name = glb.file_name().unwrap().to_string_lossy().to_string();

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: asset_dir.to_string_lossy().to_string(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            })
            .disable::<bevy::winit::WinitPlugin>(),
    )
    .add_plugins(ScheduleRunnerPlugin::run_loop(
        std::time::Duration::from_secs_f32(1.0 / 60.0),
    ))
    .add_plugins((MtoonPlugin, OutlinePlugin::EXTRUDE_VERTEX))
    .insert_resource(ClearColor(Color::srgb(0.075, 0.078, 0.094)))
    .insert_resource(Cfg { out_dir, tag, clip, frames })
    .insert_resource(GlbFile(file_name))
    .init_resource::<Progress>()
    .add_systems(Startup, setup)
    .add_systems(Update, (swap_materials, setup_player, drive_capture).chain());
    app.run();
}

fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    asset_server: Res<AssetServer>,
    glb: Res<GlbFile>,
) {
    // offscreen LDR render target (camera HDR pipeline tonemaps into this)
    let mut image = Image::new_fill(
        Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    let target = images.add(image);
    commands.insert_resource(Target(target.clone()));

    // camera: HDR + bloom + tonemapping, rendered to the offscreen image (RenderTarget is a component in 0.19)
    commands.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::from(target),
        Hdr,
        Tonemapping::AgX,
        Bloom::OLD_SCHOOL,
        AmbientLight { color: Color::srgb(0.6, 0.7, 0.95), brightness: 260.0, ..default() },
        Transform::from_xyz(0.0, 0.03, 1.95).looking_at(Vec3::new(0.0, 0.0, 0.0), Vec3::Y),
    ));

    // key light (mtoon sun) + cool fill from behind
    commands.spawn((
        DirectionalLight { illuminance: 6500.0, color: Color::srgb(1.0, 0.96, 0.9), ..default() },
        Transform::from_xyz(2.0, 3.0, 2.5).looking_at(Vec3::ZERO, Vec3::Y),
        MtoonSun,
    ));
    commands.spawn((
        DirectionalLight { illuminance: 2200.0, color: Color::srgb(0.55, 0.65, 1.0), ..default() },
        Transform::from_xyz(-2.5, 1.0, -2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // animation graph with both clips
    let clips = vec![
        asset_server.load(GltfAssetLabel::Animation(0).from_asset(glb.0.clone())),
        asset_server.load(GltfAssetLabel::Animation(1).from_asset(glb.0.clone())),
    ];
    let mut graph = AnimationGraph::new();
    let n0 = graph.add_clip(clips[0].clone(), 1.0, graph.root);
    let n1 = graph.add_clip(clips[1].clone(), 1.0, graph.root);
    let graph_handle = graphs.add(graph);
    commands.insert_resource(Anim { graph: graph_handle, nodes: vec![n0, n1], clips });

    // model on a turntable root (glTF scenes spawn via WorldAssetRoot in bevy 0.19)
    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(glb.0.clone()));
    commands.spawn((
        Transform::default(),
        Visibility::default(),
        ModelRoot,
        WorldAssetRoot(scene),
    ));
}

/// Swap every StandardMaterial mesh to a toon MtoonMaterial + add outline.
fn swap_materials(
    mut commands: Commands,
    mut progress: ResMut<Progress>,
    std_mats: Res<Assets<StandardMaterial>>,
    mut mtoon: ResMut<Assets<MtoonMaterial>>,
    q: Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
) {
    if progress.mats_done || q.is_empty() {
        return;
    }
    for (e, mat_handle) in &q {
        let (base_tex, base_col) = std_mats
            .get(&mat_handle.0)
            .map(|m| (m.base_color_texture.clone(), m.base_color))
            .unwrap_or((None, Color::WHITE));
        let m = mtoon.add(MtoonMaterial {
            base_color: base_col,
            base_color_texture: base_tex,
            // hard cel banding
            shade_factor: Color::srgb(0.32, 0.30, 0.42),
            shading_toony_factor: 0.97,
            shading_shift_factor: -0.08,
            gi_equalization_factor: 0.75,
            // parametric fresnel rim (League-ish glow)
            parametric_rim_color: Color::srgb(1.0, 0.82, 0.5),
            parametric_rim_fresnel_power: 2.8,
            parametric_rim_lift_factor: 0.10,
            rim_lighting_mix_factor: 1.0,
            // outline handled by bevy_mod_outline, not mtoon's own
            outline_mode: VrmOutlineMode::None,
            double_sided: false,
            ..default()
        });
        commands
            .entity(e)
            .remove::<MeshMaterial3d<StandardMaterial>>()
            .insert(MeshMaterial3d(m))
            .insert(OutlineVolume {
                visible: true,
                width: 6.0,
                colour: Color::srgb(0.02, 0.02, 0.04),
            });
    }
    progress.mats_done = true;
}

/// Attach the animation graph + play the requested clip (speed 0; we seek manually).
fn setup_player(
    mut commands: Commands,
    mut progress: ResMut<Progress>,
    anim: Res<Anim>,
    cfg: Res<Cfg>,
    mut players: Query<(Entity, &mut AnimationPlayer)>,
) {
    if progress.player_done {
        return;
    }
    for (e, mut player) in &mut players {
        let node = anim.nodes[cfg.clip.min(anim.nodes.len() - 1)];
        player.play(node).set_speed(0.0);
        commands.entity(e).insert(AnimationGraphHandle(anim.graph.clone()));
        progress.player_done = true;
    }
}

/// Once everything is ready: seek animation + orbit turntable + screenshot each frame.
fn drive_capture(
    mut commands: Commands,
    mut progress: ResMut<Progress>,
    cfg: Res<Cfg>,
    anim: Res<Anim>,
    target: Res<Target>,
    clips: Res<Assets<AnimationClip>>,
    mut roots: Query<&mut Transform, With<ModelRoot>>,
    mut players: Query<&mut AnimationPlayer>,
    mut exit: MessageWriter<AppExit>,
) {
    if !(progress.mats_done && progress.player_done) {
        return;
    }
    // let the pipeline warm up (skinning/material bind groups) before first capture
    if progress.warm < 6 {
        progress.warm += 1;
        return;
    }
    if progress.frame >= cfg.frames {
        // wait for async saves to flush, then exit
        if progress.saved.load(Ordering::Relaxed) >= cfg.frames || progress.warm > 6 + 300 {
            exit.write(AppExit::Success);
        }
        progress.warm += 1;
        return;
    }

    let f = progress.frame;
    let t = f as f32 / cfg.frames as f32;

    // turntable orbit
    if let Ok(mut tr) = roots.single_mut() {
        tr.rotation = Quat::from_rotation_y(t * std::f32::consts::TAU);
    }

    // deterministic animation seek (loop within clip duration)
    let node = anim.nodes[cfg.clip.min(anim.nodes.len() - 1)];
    let dur = clips.get(&anim.clips[cfg.clip]).map(|c| c.duration()).unwrap_or(1.0);
    if let Ok(mut player) = players.single_mut() {
        if let Some(active) = player.animation_mut(node) {
            active.set_seek_time((t * dur) % dur.max(0.0001));
        }
    }

    // capture this frame
    let path = cfg.out_dir.join(format!("bevy_{}_{:03}.png", cfg.tag, f));
    let counter = progress.saved.clone();
    commands
        .spawn(Screenshot::image(target.0.clone()))
        .observe(save_to_disk(path))
        .observe(move |_t: On<ScreenshotCaptured>| {
            counter.fetch_add(1, Ordering::Relaxed);
        });

    progress.frame += 1;
}

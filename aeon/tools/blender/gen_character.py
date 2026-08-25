"""Procedural rigged greybox character generator (Blender / bpy).

Run under Blender only, e.g.:

    blender --background --python tools/blender/batch.py

This module builds a single, animation-ready GREYBOX per champion, parameterized
by archetype (proportions) and faction/pantheon (material tint), then exports it
to glTF binary (`.glb`). Unlike the pure-Python fallback (box figures), this uses
metaballs merged into a smooth humanoid/creature silhouette and adds a real
armature with basic skinning, so the result is riggable/animatable.

Pipeline per champion:
  1. Build a body from metaballs (torso, head, limbs) sized by archetype.
  2. Convert the metaball to a mesh and decimate lightly (greybox density).
  3. Create a faction-tinted principled material.
  4. Build a spine + 4-limb armature and parent the mesh with automatic weights.
  5. Export the selected objects to `<out>/<slug>/model.glb`.

If `bpy` cannot be imported (not under Blender), importing this module raises
ImportError; callers (batch.py) detect that and use the fallback instead.
"""

from __future__ import annotations

import sys
from pathlib import Path

import bpy  # noqa: F401  (raises ImportError outside Blender -> handled by batch)
import mathutils  # noqa: F401

# Make the shared `aeonrift_assets` package importable when run under Blender.
_THIS = Path(__file__).resolve()
_TOOLS_DIR = _THIS.parent.parent
if str(_TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(_TOOLS_DIR))

from aeonrift_assets.roster import ARCHETYPE_PROPORTIONS, FACTION_COLORS  # noqa: E402

GENERATOR = "blender-bpy"


def _clear_scene():
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete(use_global=False)
    for block in (bpy.data.meshes, bpy.data.materials, bpy.data.armatures,
                  bpy.data.metaballs):
        for item in list(block):
            block.remove(item)


def _add_metaball_element(mball, co, radius, stiffness=2.0):
    el = mball.elements.new()
    el.co = co
    el.radius = radius
    el.stiffness = stiffness


def _build_body(archetype: str):
    """Build a metaball body, convert to mesh, return the mesh object."""
    p = ARCHETYPE_PROPORTIONS.get(archetype, ARCHETYPE_PROPORTIONS["support"])

    mball_data = bpy.data.metaballs.new("champ_mball")
    mball_data.resolution = 0.15
    mball_data.render_resolution = 0.1
    mball_obj = bpy.data.objects.new("champ_mball", mball_data)
    bpy.context.collection.objects.link(mball_obj)

    torso_h = p["torso_h"]
    leg_h = p["leg_h"]
    torso_cy = leg_h + torso_h / 2.0
    head_cy = leg_h + torso_h + p["head"] / 2.0

    # Torso (elongated blob), head, arms, legs.
    _add_metaball_element(mball_data, (0, 0, torso_cy), max(p["torso_w"], p["torso_d"]) * 0.6)
    _add_metaball_element(mball_data, (0, 0, head_cy), p["head"] * 0.6)
    arm_x = p["torso_w"] / 2.0 + p["arm_w"] / 2.0
    arm_cy = leg_h + torso_h - p["arm_h"] / 2.0
    for sx in (-arm_x, arm_x):
        _add_metaball_element(mball_data, (sx, 0, arm_cy), p["arm_w"] * 0.8)
        _add_metaball_element(mball_data, (sx, 0, arm_cy - p["arm_h"] * 0.4), p["arm_w"] * 0.6)
    leg_x = p["leg_w"] / 2.0 + 0.02
    for sx in (-leg_x, leg_x):
        _add_metaball_element(mball_data, (sx, 0, leg_h * 0.7), p["leg_w"] * 0.8)
        _add_metaball_element(mball_data, (sx, 0, leg_h * 0.25), p["leg_w"] * 0.7)
    if p.get("robe", 0.0) > 0.0:
        _add_metaball_element(mball_data, (0, 0, leg_h * 0.5),
                              (p["torso_w"] + p["robe"]) * 0.6)

    # Convert metaball -> mesh.
    bpy.context.view_layer.objects.active = mball_obj
    mball_obj.select_set(True)
    bpy.ops.object.convert(target="MESH")
    body = bpy.context.view_layer.objects.active
    body.name = "champ_body"
    return body


def _make_material(faction: str):
    color = FACTION_COLORS.get(faction, FACTION_COLORS["Neutral"])
    mat = bpy.data.materials.new(f"faction_{faction}")
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes.get("Principled BSDF")
    if bsdf:
        bsdf.inputs["Base Color"].default_value = (color[0], color[1], color[2], 1.0)
        bsdf.inputs["Roughness"].default_value = 0.85
    return mat


def _build_armature(archetype: str):
    """Create a simple spine + 4-limb armature at the origin."""
    p = ARCHETYPE_PROPORTIONS.get(archetype, ARCHETYPE_PROPORTIONS["support"])
    leg_h = p["leg_h"]
    torso_h = p["torso_h"]

    bpy.ops.object.armature_add(enter_editmode=True, location=(0, 0, 0))
    arm_obj = bpy.context.view_layer.objects.active
    arm_obj.name = "champ_rig"
    ebones = arm_obj.data.edit_bones

    root = ebones[0]
    root.name = "spine"
    root.head = (0, 0, leg_h)
    root.tail = (0, 0, leg_h + torso_h)

    def bone(name, head, tail, parent):
        b = ebones.new(name)
        b.head = head
        b.tail = tail
        b.parent = parent
        return b

    bone("head", (0, 0, leg_h + torso_h), (0, 0, leg_h + torso_h + p["head"]), root)
    arm_top = leg_h + torso_h - 0.05
    bone("arm.L", (p["torso_w"] / 2, 0, arm_top),
         (p["torso_w"] / 2 + p["arm_w"], 0, arm_top - p["arm_h"]), root)
    bone("arm.R", (-p["torso_w"] / 2, 0, arm_top),
         (-p["torso_w"] / 2 - p["arm_w"], 0, arm_top - p["arm_h"]), root)
    bone("leg.L", (p["leg_w"] / 2, 0, leg_h), (p["leg_w"] / 2, 0, 0), root)
    bone("leg.R", (-p["leg_w"] / 2, 0, leg_h), (-p["leg_w"] / 2, 0, 0), root)

    bpy.ops.object.mode_set(mode="OBJECT")
    return arm_obj


def generate(slug: str, archetype: str, faction: str, out_dir) -> str:
    """Generate one champion greybox and export it to <out_dir>/<slug>/model.glb.

    Returns the absolute path to the exported .glb.
    """
    out_dir = Path(out_dir)
    champ_dir = out_dir / slug
    champ_dir.mkdir(parents=True, exist_ok=True)
    model_path = champ_dir / "model.glb"

    _clear_scene()

    body = _build_body(archetype)
    body.data.materials.append(_make_material(faction))

    # Light decimate to keep greybox density modest.
    mod = body.modifiers.new("decimate", "DECIMATE")
    mod.ratio = 0.5
    bpy.context.view_layer.objects.active = body
    bpy.ops.object.modifier_apply(modifier=mod.name)

    rig = _build_armature(archetype)

    # Parent mesh to armature with automatic weights (basic skinning).
    bpy.ops.object.select_all(action="DESELECT")
    body.select_set(True)
    rig.select_set(True)
    bpy.context.view_layer.objects.active = rig
    bpy.ops.object.parent_set(type="ARMATURE_AUTO")

    # Export both mesh + armature to glTF binary.
    bpy.ops.object.select_all(action="DESELECT")
    body.select_set(True)
    rig.select_set(True)
    bpy.ops.export_scene.gltf(
        filepath=str(model_path),
        export_format="GLB",
        use_selection=True,
        export_apply=True,
    )
    return str(model_path)

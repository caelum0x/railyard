"""Headless Blender preview render for a GLB. NEVER opens a window.

Usage: blender --background --factory-startup --python render_preview.py -- <in.glb> <out.png>
Imports the GLB, frames it, adds a 3-point light rig, renders EEVEE to PNG.
"""
import sys
import math

import bpy
import mathutils

argv = sys.argv[sys.argv.index("--") + 1:]
in_glb, out_png = argv[0], argv[1]

# Clean scene
bpy.ops.wm.read_factory_settings(use_empty=True)

# Import GLB
bpy.ops.import_scene.gltf(filepath=in_glb)

meshes = [o for o in bpy.context.scene.objects if o.type == "MESH"]
if not meshes:
    print("NO MESH IMPORTED", file=sys.stderr)
    sys.exit(1)

# Compute combined bounding box in world space
mn = mathutils.Vector((1e9, 1e9, 1e9))
mx = mathutils.Vector((-1e9, -1e9, -1e9))
for o in meshes:
    for corner in o.bound_box:
        w = o.matrix_world @ mathutils.Vector(corner)
        mn = mathutils.Vector((min(mn[i], w[i]) for i in range(3)))
        mx = mathutils.Vector((max(mx[i], w[i]) for i in range(3)))
center = (mn + mx) / 2.0
size = (mx - mn)
radius = max(size.length / 2.0, 0.001)

# Camera
cam_data = bpy.data.cameras.new("Cam")
cam = bpy.data.objects.new("Cam", cam_data)
bpy.context.scene.collection.objects.link(cam)
bpy.context.scene.camera = cam
dist = radius * 3.0
cam_dir = mathutils.Vector((0.6, -1.0, 0.45)).normalized()
cam.location = center + cam_dir * dist
# Point camera at center
direction = center - cam.location
cam.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()
cam_data.lens = 50

# 3-point lighting
def add_light(name, loc, energy, kind="AREA", size=None):
    ld = bpy.data.lights.new(name, kind)
    ld.energy = energy
    if size and hasattr(ld, "size"):
        ld.size = size
    lo = bpy.data.objects.new(name, ld)
    lo.location = center + mathutils.Vector(loc) * (radius * 3)
    d = center - lo.location
    lo.rotation_euler = d.to_track_quat("-Z", "Y").to_euler()
    bpy.context.scene.collection.objects.link(lo)

add_light("Key", (1.0, -1.0, 1.0), 1200, size=radius * 4)
add_light("Fill", (-1.2, -0.6, 0.5), 500, size=radius * 5)
add_light("Rim", (0.0, 1.2, 0.8), 700, size=radius * 3)

# World background (soft neutral)
world = bpy.data.worlds.new("W")
bpy.context.scene.world = world
world.use_nodes = True
bg = world.node_tree.nodes.get("Background")
if bg:
    bg.inputs[0].default_value = (0.05, 0.05, 0.06, 1.0)
    bg.inputs[1].default_value = 0.4

# Render settings — EEVEE, headless PNG
scene = bpy.context.scene
# Blender 4.2+ names it 'BLENDER_EEVEE_NEXT'; older is 'BLENDER_EEVEE'
for eng in ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"):
    try:
        scene.render.engine = eng
        break
    except Exception:
        continue
scene.render.resolution_x = 768
scene.render.resolution_y = 1024
scene.render.film_transparent = False
scene.render.image_settings.file_format = "PNG"
scene.render.filepath = out_png
try:
    scene.eevee.taa_render_samples = 32
except Exception:
    pass

bpy.ops.render.render(write_still=True)
print("RENDERED", out_png)

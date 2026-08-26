"""Headless Blender studio render of a single champion glTF (.glb).

Usage (fully headless, no window):
    blender --background --python render_champion.py -- <model.glb> <out.png> \
        "<Name>" "<Faction>" "<Archetype>" "#hexcolor"

Imports the glTF, drops it on a neutral studio floor, frames it with a camera,
lights it with a soft 3-point rig, and renders an 800x1000 portrait via EEVEE.
These are greybox procedural models -- the rig just presents them cleanly.
"""

import bpy
import sys
import math
from mathutils import Vector


def argv_after_ddash():
    argv = sys.argv
    if "--" in argv:
        return argv[argv.index("--") + 1:]
    return []


def clear_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def hex_to_rgb(h, default=(0.55, 0.55, 0.58)):
    try:
        h = h.lstrip("#")
        if len(h) != 6:
            return (*default, 1.0)
        r = int(h[0:2], 16) / 255.0
        g = int(h[2:4], 16) / 255.0
        b = int(h[4:6], 16) / 255.0
        return (r, g, b, 1.0)
    except Exception:
        return (*default, 1.0)


def import_glb(path):
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=path)
    after = set(bpy.data.objects)
    return [o for o in (after - before)]


def world_bbox(objects):
    mins = Vector((math.inf, math.inf, math.inf))
    maxs = Vector((-math.inf, -math.inf, -math.inf))
    found = False
    for o in objects:
        if o.type != "MESH":
            continue
        found = True
        for corner in o.bound_box:
            wc = o.matrix_world @ Vector(corner)
            for i in range(3):
                mins[i] = min(mins[i], wc[i])
                maxs[i] = max(maxs[i], wc[i])
    if not found:
        # fall back to object origins
        for o in objects:
            wc = o.matrix_world.translation
            for i in range(3):
                mins[i] = min(mins[i], wc[i])
                maxs[i] = max(maxs[i], wc[i])
    return mins, maxs


def make_floor(z, color):
    mesh = bpy.data.meshes.new("floor")
    obj = bpy.data.objects.new("Floor", mesh)
    bpy.context.collection.objects.link(obj)
    import bmesh
    bm = bmesh.new()
    s = 40.0
    bmesh.ops.create_grid(bm, x_segments=1, y_segments=1, size=s)
    bm.to_mesh(mesh)
    bm.free()
    obj.location = (0, 0, z)
    mat = bpy.data.materials.new("floor_mat")
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes.get("Principled BSDF")
    if bsdf:
        bsdf.inputs["Base Color"].default_value = (0.14, 0.15, 0.18, 1.0)
        if "Roughness" in bsdf.inputs:
            bsdf.inputs["Roughness"].default_value = 0.85
    obj.data.materials.append(mat)
    return obj


def tint_untextured(objects, color):
    """Give any mesh that has no material a neutral tint so greybox reads well."""
    tint = bpy.data.materials.new("champion_tint")
    tint.use_nodes = True
    bsdf = tint.node_tree.nodes.get("Principled BSDF")
    if bsdf:
        bsdf.inputs["Base Color"].default_value = color
        if "Roughness" in bsdf.inputs:
            bsdf.inputs["Roughness"].default_value = 0.5
        if "Metallic" in bsdf.inputs:
            bsdf.inputs["Metallic"].default_value = 0.1
    for o in objects:
        if o.type != "MESH":
            continue
        if len(o.data.materials) == 0:
            o.data.materials.append(tint)


def add_light(name, kind, loc, energy, size=5.0, color=(1, 1, 1)):
    data = bpy.data.lights.new(name, type=kind)
    data.energy = energy
    data.color = color
    if kind == "AREA":
        data.size = size
    obj = bpy.data.objects.new(name, data)
    obj.location = loc
    bpy.context.collection.objects.link(obj)
    return obj


def point_at(obj, target):
    direction = Vector(target) - obj.location
    obj.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()


def render_one(glb, out, name="Champion", faction="", archetype="", hexcol=""):
    color = hex_to_rgb(hexcol)

    clear_scene()

    try:
        objs = import_glb(glb)
    except Exception as e:
        print("GALLERY_ERROR: gltf import failed for %s: %r" % (name, e))
        return False

    if not objs:
        print("GALLERY_ERROR: no objects imported for %s" % name)
        return False

    tint_untextured(objs, color)

    # Seat the model on z=0 so it never clips through the floor. Only move the
    # imported roots; children inherit the transform.
    mins, maxs = world_bbox(objs)
    obj_set = set(objs)
    roots = [o for o in objs if o.parent not in obj_set]
    for r in roots:
        r.location.z -= mins.z
    bpy.context.view_layer.update()

    mins, maxs = world_bbox(objs)
    center = (mins + maxs) * 0.5
    size = maxs - mins
    height = max(size.z, 0.001)
    radius = max(size.x, size.y, size.z, 0.5)

    # Floor sits at the model's feet (z=0).
    make_floor(0.0, color)

    # Neutral studio world (soft ambient).
    world = bpy.data.worlds.new("studio")
    world.use_nodes = True
    bg = world.node_tree.nodes.get("Background")
    if bg:
        bg.inputs["Color"].default_value = (0.05, 0.055, 0.07, 1.0)
        bg.inputs["Strength"].default_value = 1.0
    bpy.context.scene.world = world

    # Camera: frame the model tightly to fill the 800x1000 portrait. Distance is
    # derived from the vertical FOV so the model height fills ~78% of the frame.
    cam_data = bpy.data.cameras.new("cam")
    cam_data.lens = 50
    cam_data.sensor_fit = "VERTICAL"
    cam_data.sensor_height = 36.0
    cam = bpy.data.objects.new("Camera", cam_data)
    bpy.context.collection.objects.link(cam)
    vfov = 2.0 * math.atan((cam_data.sensor_height * 0.5) / cam_data.lens)
    framing = height * 1.18  # target model height + small padding
    dist = (framing * 0.5) / math.tan(vfov * 0.5)
    look = Vector((center.x, center.y, center.z))
    cam.location = (center.x + dist * 0.28, center.y - dist * 0.96, look.z + height * 0.04)
    point_at(cam, look)
    bpy.context.scene.camera = cam

    # 3-point soft rig scaled to the model.
    key = add_light("Key", "AREA", (center.x + radius * 2.5, center.y - radius * 2.5, center.z + height * 1.6),
                    energy=max(300.0, radius * 220.0), size=radius * 2.0, color=(1.0, 0.97, 0.92))
    point_at(key, center)
    fill = add_light("Fill", "AREA", (center.x - radius * 3.0, center.y - radius * 1.5, center.z + height * 0.8),
                     energy=max(120.0, radius * 90.0), size=radius * 3.0, color=(0.85, 0.9, 1.0))
    point_at(fill, center)
    rim = add_light("Rim", "AREA", (center.x, center.y + radius * 3.0, center.z + height * 1.8),
                    energy=max(220.0, radius * 160.0), size=radius * 1.5, color=(1.0, 1.0, 1.0))
    point_at(rim, center)

    scene = bpy.context.scene
    # Pick an EEVEE engine id that exists in this Blender build.
    engines = {i.identifier for i in bpy.types.RenderSettings.bl_rna.properties["engine"].enum_items}
    for eng in ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"):
        if eng in engines:
            scene.render.engine = eng
            break
    scene.render.resolution_x = 800
    scene.render.resolution_y = 1000
    scene.render.film_transparent = False
    scene.render.image_settings.file_format = "PNG"
    scene.render.filepath = out
    try:
        scene.eevee.taa_render_samples = 64
    except Exception:
        pass

    bpy.ops.render.render(write_still=True)
    print("GALLERY_OK: %s -> %s (h=%.2f r=%.2f)" % (name, out, height, radius))
    return True


def main():
    args = argv_after_ddash()
    if len(args) < 2:
        print("GALLERY_ERROR: need <glb> <out.png>")
        sys.exit(2)
    ok = render_one(
        args[0], args[1],
        args[2] if len(args) > 2 else "Champion",
        args[3] if len(args) > 3 else "",
        args[4] if len(args) > 4 else "",
        args[5] if len(args) > 5 else "",
    )
    sys.exit(0 if ok else 3)


if __name__ == "__main__":
    main()

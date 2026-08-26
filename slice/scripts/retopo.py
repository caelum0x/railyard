"""Retopo/cleanup: TRELLIS mesh -> game-ready tri budget, UV + PBR textures preserved.
Path: merge-by-distance + delete-loose + recalc-normals + Decimate(collapse) with UV preserve.
(Full quad retopology via QuadriFlow was evaluated but skipped: it discards the baked UVs/textures,
 which would force a texture re-bake. Collapse-decimate keeps the original UV atlas + PBR maps intact.)
"""
import bpy, sys, json, bmesh

argv = sys.argv[sys.argv.index("--")+1:]
src, dst = argv[0], argv[1]

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=src)

meshes = [o for o in bpy.data.objects if o.type=='MESH']
bpy.ops.object.select_all(action='DESELECT')
for o in meshes: o.select_set(True)
bpy.context.view_layer.objects.active = meshes[0]
if len(meshes) > 1:
    bpy.ops.object.join()
ob = bpy.context.view_layer.objects.active
ob.name = "Anubis"

def tris(o):
    o.data.calc_loop_triangles()
    return len(o.data.loop_triangles)

before = tris(ob)

# --- cleanup ---
bpy.ops.object.mode_set(mode='EDIT')
bm = bmesh.from_edit_mesh(ob.data)
bmesh.ops.remove_doubles(bm, verts=bm.verts, dist=0.0005)
bmesh.update_edit_mesh(ob.data)
bpy.ops.mesh.select_all(action='SELECT')
bpy.ops.mesh.delete_loose()
bpy.ops.mesh.normals_make_consistent(inside=False)
bpy.ops.object.mode_set(mode='OBJECT')

after_clean = tris(ob)

# --- decimate collapse, UV-boundary preserving, target ~10.5k tris ---
target = 10500
ratio = min(1.0, target / max(1, after_clean))
mod = ob.modifiers.new("Decimate", 'DECIMATE')
mod.decimate_type = 'COLLAPSE'
mod.ratio = ratio
mod.use_collapse_triangulate = True
# preserve UV seams / boundaries so texture atlas stays valid
if hasattr(mod, "delimit"):
    mod.delimit = {'SEAM', 'UV'}
bpy.ops.object.modifier_apply(modifier="Decimate")

after = tris(ob)

# ensure single mesh at origin, +Z up (already), reasonable scale
bpy.ops.object.select_all(action='DESELECT')
ob.select_set(True); bpy.context.view_layer.objects.active = ob

bpy.ops.export_scene.gltf(
    filepath=dst, export_format='GLB',
    use_selection=True,
    export_yup=True,
    export_apply=True,
    export_materials='EXPORT',
    export_image_format='AUTO',
)
print("RETOPO_JSON=" + json.dumps({
    "tris_before": before, "tris_after_clean": after_clean,
    "tris_after": after, "ratio": round(ratio,3),
    "verts_after": len(ob.data.vertices), "uv_layers": len(ob.data.uv_layers),
    "materials": [m.name for m in ob.data.materials if m],
}))

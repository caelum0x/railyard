import bpy, sys, json

argv = sys.argv[sys.argv.index("--")+1:]
src = argv[0]

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=src)

info = {"objects": [], "total_tris": 0, "total_verts": 0, "materials": set()}
import bmesh
for ob in bpy.data.objects:
    if ob.type == 'MESH':
        me = ob.data
        me.calc_loop_triangles()
        tris = len(me.loop_triangles)
        info["objects"].append({
            "name": ob.name,
            "verts": len(me.vertices),
            "tris": tris,
            "materials": [m.name for m in me.materials],
            "dims": [round(d,3) for d in ob.dimensions],
            "loc": [round(c,3) for c in ob.location],
        })
        info["total_tris"] += tris
        info["total_verts"] += len(me.vertices)
        for m in me.materials:
            if m: info["materials"].add(m.name)

# world bounds
import mathutils
mn = [1e9,1e9,1e9]; mx=[-1e9,-1e9,-1e9]
for ob in bpy.data.objects:
    if ob.type=='MESH':
        for v in ob.bound_box:
            w = ob.matrix_world @ mathutils.Vector(v)
            for i in range(3):
                mn[i]=min(mn[i],w[i]); mx[i]=max(mx[i],w[i])
info["materials"]=sorted(info["materials"])
info["world_min"]=[round(x,3) for x in mn]
info["world_max"]=[round(x,3) for x in mx]
info["world_size"]=[round(mx[i]-mn[i],3) for i in range(3)]
print("INSPECT_JSON="+json.dumps(info))

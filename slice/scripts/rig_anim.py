"""Auto-rig a humanoid skeleton (Mixamo-compatible bone names) onto the retopo Anubis,
skin with automatic weights, author procedural IDLE + WALK clips, and bake BOTH into ONE GLB.
+Y up, quaternion rotations (glTF native), no Draco. Fully headless.
"""
import bpy, sys, json, math
from mathutils import Vector, Euler

argv = sys.argv[sys.argv.index("--")+1:]
src, dst = argv[0], argv[1]

bpy.ops.wm.read_factory_settings(use_empty=True)
sc = bpy.context.scene
sc.render.fps = 24

bpy.ops.import_scene.gltf(filepath=src)
mesh = next(o for o in bpy.data.objects if o.type=='MESH')
mesh.name = "Anubis"
# glTF import applies +Y-up->+Z-up conversion; ensure transforms applied
bpy.ops.object.select_all(action='DESELECT')
mesh.select_set(True); bpy.context.view_layer.objects.active = mesh
bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)
# weld split verts (glTF stores per-corner attrs -> unwelded); needed for bone-heat weighting
import bmesh
bpy.ops.object.mode_set(mode='EDIT')
_bm = bmesh.from_edit_mesh(mesh.data)
bmesh.ops.remove_doubles(_bm, verts=_bm.verts, dist=0.0002)
bmesh.update_edit_mesh(mesh.data)
bpy.ops.mesh.normals_make_consistent(inside=False)
bpy.ops.object.mode_set(mode='OBJECT')

# ---------------- bone skeleton (world Z-up, char faces -Y) ----------------
# name: (head xyz, tail xyz, parent, connect)
Z = lambda v: Vector(v)
S = {}
def add(name, head, tail, parent, connect):
    S[name] = dict(head=Z(head), tail=Z(tail), parent=parent, connect=connect)

# spine
add("Hips",   (0,0.00,-0.06), (0,0.00, 0.02), None, False)
add("Spine",  (0,0.00, 0.02), (0,0.00, 0.09), "Hips", True)
add("Spine1", (0,0.00, 0.09), (0,0.00, 0.15), "Spine", True)
add("Spine2", (0,0.00, 0.15), (0,0.00, 0.22), "Spine1", True)
add("Neck",   (0,0.00, 0.24), (0,0.00, 0.29), "Spine2", False)
add("Head",   (0,0.00, 0.29), (0,0.015,0.40), "Neck", True)
# arms (A-pose: down & out).  Left = +X
for s, sx in (("Left",1.0), ("Right",-1.0)):
    add(s+"Shoulder", (sx*0.03,0,0.175), (sx*0.10,0,0.155), "Spine2", False)
    add(s+"Arm",      (sx*0.10,0,0.155), (sx*0.20,0.01,0.075), s+"Shoulder", True)
    add(s+"ForeArm",  (sx*0.20,0.01,0.075), (sx*0.29,0.0,0.015), s+"Arm", True)
    add(s+"Hand",     (sx*0.29,0.0,0.015), (sx*0.335,-0.01,-0.02), s+"ForeArm", True)
    # legs.  Left = +X
    add(s+"UpLeg",    (sx*0.065,0,-0.09), (sx*0.065,0.01,-0.28), "Hips", False)
    add(s+"Leg",      (sx*0.065,0.01,-0.28), (sx*0.07,0.0,-0.455), s+"UpLeg", True)
    add(s+"Foot",     (sx*0.07,0.0,-0.455), (sx*0.07,-0.07,-0.495), s+"Leg", True)
    add(s+"ToeBase",  (sx*0.07,-0.07,-0.495), (sx*0.07,-0.125,-0.50), s+"Foot", True)

arm = bpy.data.armatures.new("Rig")
rig = bpy.data.objects.new("Armature", arm)
sc.collection.objects.link(rig)
bpy.context.view_layer.objects.active = rig
bpy.ops.object.mode_set(mode='EDIT')
order = ["Hips","Spine","Spine1","Spine2","Neck","Head"]
for s in ("Left","Right"):
    order += [s+"Shoulder",s+"Arm",s+"ForeArm",s+"Hand",s+"UpLeg",s+"Leg",s+"Foot",s+"ToeBase"]
eb = arm.edit_bones
for name in order:
    d = S[name]
    b = eb.new(name)
    b.head = d["head"]; b.tail = d["tail"]
    if d["parent"]:
        b.parent = eb[d["parent"]]
        b.use_connect = d["connect"]
bpy.ops.object.mode_set(mode='OBJECT')

# ---------------- skin: automatic weights (fallback envelope) ----------------
bpy.ops.object.select_all(action='DESELECT')
mesh.select_set(True); rig.select_set(True)
bpy.context.view_layer.objects.active = rig
skin = "auto"
try:
    bpy.ops.object.parent_set(type='ARMATURE_AUTO')
except RuntimeError:
    skin = "envelope"
    bpy.ops.object.parent_set(type='ARMATURE_ENVELOPE')
# verify skinning produced weights
has_w = any(len(v.groups) for v in mesh.data.vertices)
if not has_w:
    skin = "envelope"
    bpy.ops.object.select_all(action='DESELECT')
    mesh.select_set(True); rig.select_set(True)
    bpy.context.view_layer.objects.active = rig
    bpy.ops.object.parent_set(type='ARMATURE_ENVELOPE')

# ---------------- procedural animation ----------------
for pb in rig.pose.bones:
    pb.rotation_mode = 'XYZ'

def key(action, frame):
    """insert keyframes for all posebones (rot) + hips (loc) at frame from current pose"""
    for pb in rig.pose.bones:
        pb.keyframe_insert("rotation_euler", frame=frame, group=pb.name)
    rig.pose.bones["Hips"].keyframe_insert("location", frame=frame, group="Hips")

def new_action(name):
    act = bpy.data.actions.new(name)
    act.use_fake_user = True
    if not rig.animation_data:
        rig.animation_data_create()
    rig.animation_data.action = act
    return act

def reset_pose():
    for pb in rig.pose.bones:
        pb.rotation_euler = Euler((0,0,0))
        pb.location = Vector((0,0,0))

D = math.radians

# ---- IDLE : 48 frames, gentle breathe + weight shift ----
idle = new_action("Idle")
N = 48
for f in range(0, N+1, 2):
    t = f / N
    ph = 2*math.pi*t
    reset_pose()
    rig.pose.bones["Hips"].location = Vector((0, 0, 0.012*math.sin(ph)))
    rig.pose.bones["Spine"].rotation_euler.x = D(2.0)*math.sin(ph)
    rig.pose.bones["Spine1"].rotation_euler.x = D(1.5)*math.sin(ph)
    rig.pose.bones["Spine2"].rotation_euler.x = D(1.0)*math.sin(ph)
    rig.pose.bones["Head"].rotation_euler.x = D(-1.5)*math.sin(ph)
    rig.pose.bones["Head"].rotation_euler.z = D(2.0)*math.sin(ph*0.5)
    for s,sgn in (("Left",1),("Right",-1)):
        rig.pose.bones[s+"Arm"].rotation_euler.x = D(3.0)*math.sin(ph)
        rig.pose.bones[s+"ForeArm"].rotation_euler.x = D(2.0)*math.sin(ph)
    key(idle, f)

# ---- WALK : 24 frames, stride + arm counter-swing + hip bob ----
walk = new_action("Walk")
M = 24
for f in range(0, M+1, 1):
    t = f / M
    ph = 2*math.pi*t
    reset_pose()
    # hip bob twice per cycle + slight roll/twist
    rig.pose.bones["Hips"].location = Vector((0, 0, 0.02*abs(math.sin(ph)) - 0.01))
    rig.pose.bones["Hips"].rotation_euler.y = D(4.0)*math.sin(ph)
    rig.pose.bones["Hips"].rotation_euler.z = D(6.0)*math.sin(ph)
    rig.pose.bones["Spine1"].rotation_euler.z = D(-4.0)*math.sin(ph)
    rig.pose.bones["Spine2"].rotation_euler.x = D(4.0)
    # legs: opposite phase stride about local X
    for s,sgn in (("Left",1),("Right",-1)):
        swing = math.sin(ph + (0 if s=="Left" else math.pi))
        up = rig.pose.bones[s+"UpLeg"]; lo = rig.pose.bones[s+"Leg"]; ft = rig.pose.bones[s+"Foot"]
        up.rotation_euler.x = D(28.0)*swing
        # knee bends when leg swings back
        lo.rotation_euler.x = D(-max(0.0, -swing))*35.0 - D(8.0)
        ft.rotation_euler.x = D(12.0)*swing + D(6.0)
        # arms counter-swing (opposite the same-side leg)
        ar = rig.pose.bones[s+"Arm"]; fa = rig.pose.bones[s+"ForeArm"]
        ar.rotation_euler.x = D(-18.0)*swing
        fa.rotation_euler.x = D(10.0)*(0.5+0.5*swing) + D(6.0)
    key(walk, f)

reset_pose()

# ---------------- export ONE glb with both clips ----------------
# stash both actions into NLA so NLA_TRACKS export emits both animations
rig.animation_data.action = None
for act in (idle, walk):
    tr = rig.animation_data.nla_tracks.new()
    tr.name = act.name
    tr.strips.new(act.name, int(act.frame_range[0]), act)

bpy.ops.object.select_all(action='DESELECT')
mesh.select_set(True); rig.select_set(True)
bpy.context.view_layer.objects.active = rig

bpy.ops.export_scene.gltf(
    filepath=dst, export_format='GLB',
    use_selection=True,
    export_yup=True,
    export_apply=False,
    export_materials='EXPORT',
    export_image_format='AUTO',
    export_animations=True,
    export_animation_mode='NLA_TRACKS',
    export_nla_strips=True,
    export_bake_animation=True,
    export_skins=True,
    export_def_bones=False,
)

print("RIG_JSON=" + json.dumps({
    "skin": skin, "bones": len(rig.pose.bones),
    "bone_names": [b.name for b in rig.pose.bones],
    "actions": [a.name for a in bpy.data.actions],
    "weighted_verts": sum(1 for v in mesh.data.vertices if len(v.groups)),
    "total_verts": len(mesh.data.vertices),
}))

"""Headless Blender EEVEE renderer for the Anubis slice.
Modes:
  still  <glb> <out.png> [pbr|wire|bones]   -- single 3/4 hero still
  cel    <glb> <out_prefix> <nframes> <clip Idle|Walk>  -- cel+outline turntable frames
Cel look = banded (Shader-to-RGB + stepped ColorRamp) diffuse * base texture, fresnel rim add,
inverted-hull black outline, dark stage + key/rim lights + Bloom-style glare compositor.
This is the *Blender EEVEE proxy* of the in-engine target (labelled as such in SLICE.html).
"""
import bpy, sys, math, os
from mathutils import Vector

A = sys.argv[sys.argv.index("--")+1:]
MODE = A[0]

def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)

def import_glb(p):
    bpy.ops.import_scene.gltf(filepath=p)
    return [o for o in bpy.data.objects if o.type=='MESH']

def scene_common(res=1024, transparent=False):
    sc = bpy.context.scene
    sc.render.engine = 'BLENDER_EEVEE'
    sc.render.resolution_x = res
    sc.render.resolution_y = res
    sc.render.film_transparent = transparent
    try: sc.eevee.use_raytracing = True
    except Exception: pass
    sc.view_settings.view_transform = 'AgX'
    return sc

def add_world(sc, top=(0.05,0.06,0.09), strength=1.0):
    w = bpy.data.worlds.new("W"); sc.world = w
    w.use_nodes = True
    bg = w.node_tree.nodes.get("Background")
    bg.inputs[0].default_value = (*top, 1.0)
    bg.inputs[1].default_value = strength

def look_camera(sc, target=Vector((0,0,0.0)), pos=Vector((1.35,-2.05,0.42)), lens=60):
    cam_d = bpy.data.cameras.new("Cam"); cam_d.lens = lens
    cam = bpy.data.objects.new("Cam", cam_d); sc.collection.objects.link(cam)
    sc.camera = cam
    cam.location = pos
    d = (target - pos).normalized()
    cam.rotation_euler = d.to_track_quat('-Z','Y').to_euler()
    return cam

def key_lights(sc):
    def lamp(name, energy, color, loc, angle=0.6):
        ld = bpy.data.lights.new(name, 'SUN'); ld.energy = energy; ld.color = color
        ld.angle = angle
        o = bpy.data.objects.new(name, ld); sc.collection.objects.link(o)
        o.location = loc
        o.rotation_euler = (Vector((0,0,0))-Vector(loc)).to_track_quat('-Z','Y').to_euler()
        return o
    lamp("Key", 5.0, (1.0,0.95,0.85), (2.5,-2.0,3.0))
    lamp("Rim", 4.0, (0.5,0.65,1.0), (-2.6,1.6,1.4))
    lamp("Fill",1.3, (0.8,0.85,1.0), (-1.5,-2.2,0.6))

def glare_comp(sc, threshold=0.85, size=7):
    sc.use_nodes = True
    nt = getattr(sc, "node_tree", None) or getattr(sc, "compositing_node_group", None)
    if nt is None:
        return
    for n in list(nt.nodes): nt.nodes.remove(n)
    rl = nt.nodes.new("CompositorNodeRLayers")
    glare = nt.nodes.new("CompositorNodeGlare")
    glare.glare_type = 'BLOOM' if hasattr(glare,'glare_type') else 'FOG_GLOW'
    try: glare.glare_type = 'BLOOM'
    except Exception: glare.glare_type = 'FOG_GLOW'
    glare.threshold = threshold
    try: glare.size = size
    except Exception: pass
    comp = nt.nodes.new("CompositorNodeComposite")
    nt.links.new(rl.outputs['Image'], glare.inputs['Image'])
    nt.links.new(glare.outputs['Image'], comp.inputs['Image'])

# ---------- cel material ----------
def make_cel_material(name, base_tex, base_color=(0.5,0.5,0.5,1)):
    m = bpy.data.materials.new(name); m.use_nodes = True
    nt = m.node_tree; nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    emit = nt.nodes.new("ShaderNodeEmission")
    # diffuse -> shader to rgb -> stepped ramp = cel bands
    diff = nt.nodes.new("ShaderNodeBsdfDiffuse")
    s2r = nt.nodes.new("ShaderNodeShaderToRGB")
    ramp = nt.nodes.new("ShaderNodeValToRGB")
    ramp.color_ramp.interpolation = 'CONSTANT'
    e = ramp.color_ramp.elements
    # 3 flat cel bands, lifted so shadow side stays readable/colourful
    e[0].position = 0.0;  e[0].color = (0.55,0.55,0.62,1)
    e[1].position = 0.40; e[1].color = (0.82,0.82,0.88,1)
    e.new(0.68); e[2].color = (1.12,1.12,1.12,1)
    # base color source
    if base_tex is not None:
        tex = nt.nodes.new("ShaderNodeTexImage"); tex.image = base_tex
        tex.interpolation = 'Closest'
        base_out = tex.outputs['Color']
    else:
        rgb = nt.nodes.new("ShaderNodeRGB"); rgb.outputs[0].default_value = base_color
        base_out = rgb.outputs[0]
    nt.links.new(base_out, diff.inputs['Color'])
    nt.links.new(diff.outputs['BSDF'], s2r.inputs['Shader'])
    nt.links.new(s2r.outputs['Color'], ramp.inputs['Fac'])
    # banded light * base color
    mul = nt.nodes.new("ShaderNodeMixRGB"); mul.blend_type='MULTIPLY'; mul.inputs['Fac'].default_value=1.0
    nt.links.new(ramp.outputs['Color'], mul.inputs['Color1'])
    nt.links.new(base_out, mul.inputs['Color2'])
    # fresnel rim add
    fres = nt.nodes.new("ShaderNodeFresnel"); fres.inputs['IOR'].default_value = 1.35
    rimramp = nt.nodes.new("ShaderNodeValToRGB"); rimramp.color_ramp.interpolation='CONSTANT'
    re = rimramp.color_ramp.elements; re[0].position=0.55; re[0].color=(0,0,0,1); re[1].position=0.72; re[1].color=(1,0.82,0.5,1)
    nt.links.new(fres.outputs['Fac'], rimramp.inputs['Fac'])
    add = nt.nodes.new("ShaderNodeMixRGB"); add.blend_type='ADD'; add.inputs['Fac'].default_value=0.9
    nt.links.new(mul.outputs['Color'], add.inputs['Color1'])
    nt.links.new(rimramp.outputs['Color'], add.inputs['Color2'])
    nt.links.new(add.outputs['Color'], emit.inputs['Color'])
    emit.inputs['Strength'].default_value = 1.0
    nt.links.new(emit.outputs['Emission'], out.inputs['Surface'])
    m.use_backface_culling = False
    return m

def enable_freestyle(sc, thickness=2.4, color=(0,0,0)):
    """Toon silhouette/crease outline via Freestyle (robust, headless-friendly)."""
    sc.render.use_freestyle = True
    vl = bpy.context.view_layer
    vl.use_freestyle = True
    fs = vl.freestyle_settings
    fs.crease_angle = math.radians(130)
    ls = fs.linesets[0] if len(fs.linesets) else fs.linesets.new("LS")
    ls.select_silhouette = True
    ls.select_border = True
    ls.select_crease = True
    ls.select_edge_mark = False
    st = ls.linestyle
    if st is None:
        st = bpy.data.linestyles.new("ToonLine")
        ls.linestyle = st
    st.color = color
    st.thickness = thickness

def get_base_tex(mesh):
    for m in mesh.data.materials:
        if not m or not m.use_nodes: continue
        for n in m.node_tree.nodes:
            if n.type=='TEX_IMAGE' and n.image:
                return n.image
    return None

# ================= MODES =================
if MODE == 'still':
    glb, out, style = A[1], A[2], (A[3] if len(A)>3 else 'pbr')
    reset(); meshes = import_glb(glb)
    sc = scene_common(res=900, transparent=True)
    add_world(sc); key_lights(sc); look_camera(sc)
    if style == 'wire':
        for ob in meshes:
            mod = ob.modifiers.new("wire",'WIREFRAME'); mod.thickness=0.0016
            mat = bpy.data.materials.new("wm"); mat.use_nodes=True
            bsdf=mat.node_tree.nodes.get("Principled BSDF")
            bsdf.inputs['Base Color'].default_value=(0.1,0.85,0.9,1)
            bsdf.inputs['Emission Color'].default_value=(0.1,0.85,0.9,1)
            bsdf.inputs['Emission Strength'].default_value=1.2
            ob.data.materials.clear(); ob.data.materials.append(mat)
    elif style == 'skel':
        arm = next((o for o in bpy.data.objects if o.type=='ARMATURE'), None)
        # ghost the mesh
        for ob in meshes:
            gm = bpy.data.materials.new("ghost"); gm.use_nodes=True
            nt=gm.node_tree; nt.nodes.clear()
            o=nt.nodes.new("ShaderNodeOutputMaterial"); bs=nt.nodes.new("ShaderNodeBsdfPrincipled")
            bs.inputs['Base Color'].default_value=(0.30,0.45,0.62,1)
            bs.inputs['Alpha'].default_value=0.14
            nt.links.new(bs.outputs['BSDF'], o.inputs['Surface'])
            gm.surface_render_method='BLENDED' if hasattr(gm,'surface_render_method') else gm.blend_method
            try: gm.blend_method='BLEND'
            except Exception: pass
            ob.data.materials.clear(); ob.data.materials.append(gm)
        # emissive bone geometry
        bonemat = bpy.data.materials.new("bone"); bonemat.use_nodes=True
        nt=bonemat.node_tree; nt.nodes.clear()
        o=nt.nodes.new("ShaderNodeOutputMaterial"); em=nt.nodes.new("ShaderNodeEmission")
        em.inputs['Color'].default_value=(1.0,0.55,0.12,1); em.inputs['Strength'].default_value=3.0
        nt.links.new(em.outputs['Emission'], o.inputs['Surface'])
        jointmat = bpy.data.materials.new("joint"); jointmat.use_nodes=True
        nt=jointmat.node_tree; nt.nodes.clear()
        o=nt.nodes.new("ShaderNodeOutputMaterial"); em=nt.nodes.new("ShaderNodeEmission")
        em.inputs['Color'].default_value=(0.3,0.9,1.0,1); em.inputs['Strength'].default_value=4.0
        nt.links.new(em.outputs['Emission'], o.inputs['Surface'])
        if arm:
            mw = arm.matrix_world
            for b in arm.data.bones:
                p0 = mw @ b.head_local; p1 = mw @ b.tail_local
                vec = p1 - p0; L = vec.length
                if L < 1e-5: continue
                bpy.ops.mesh.primitive_cylinder_add(radius=0.007, depth=L, vertices=8)
                cyl = bpy.context.active_object
                cyl.location = (p0 + p1) / 2
                cyl.rotation_euler = vec.to_track_quat('Z','Y').to_euler()
                cyl.data.materials.append(bonemat)
                cyl.show_in_front = True
                bpy.ops.mesh.primitive_uv_sphere_add(radius=0.014, location=p0)
                bpy.context.active_object.data.materials.append(jointmat)
    elif style == 'bones':
        # keep mesh semi and show armature octahedral bones
        arm = next((o for o in bpy.data.objects if o.type=='ARMATURE'), None)
        for ob in meshes:
            for m in ob.data.materials:
                if m and m.use_nodes:
                    b=m.node_tree.nodes.get("Principled BSDF")
                    if b: b.inputs['Alpha'].default_value=0.35
                    m.blend_method='BLEND'
        if arm:
            arm.show_in_front=True
            arm.data.display_type='OCTAHEDRAL'
            # emissive bone overlay: add a wire empty? bones render via viewport only.
    sc.render.filepath = out
    bpy.ops.render.render(write_still=True)
    print("STILL_DONE="+out)

elif MODE == 'cel':
    glb, prefix, nframes, clip = A[1], A[2], int(A[3]), A[4]
    use_outline = (len(A) < 6) or (A[5] != 'noout')
    reset(); meshes = import_glb(glb)
    sc = scene_common(res=1024, transparent=False)
    add_world(sc, top=(0.03,0.035,0.05), strength=1.0)
    key_lights(sc)
    try: glare_comp(sc)
    except Exception as ex: print("GLARE_SKIP", ex)
    # assign the requested action to the armature
    arm = next((o for o in bpy.data.objects if o.type=='ARMATURE'), None)
    act = bpy.data.actions.get(clip)
    if arm and act:
        if not arm.animation_data: arm.animation_data_create()
        # clear NLA so only the chosen action drives the pose
        for t in list(arm.animation_data.nla_tracks): arm.animation_data.nla_tracks.remove(t)
        arm.animation_data.action = act
    dur = int(act.frame_range[1]) if act else 1
    # cel material + outline per mesh
    for ob in meshes:
        tex = get_base_tex(ob)
        cel = make_cel_material("cel_"+ob.name, tex)
        ob.data.materials.clear(); ob.data.materials.append(cel)
    if use_outline:
        enable_freestyle(sc, thickness=2.4)
    # fixed 3/4 camera + fixed lights; rotate the model on a turntable empty so it stays lit
    look_camera(sc, target=Vector((0,0,0.02)), pos=Vector((1.15,-2.25,0.45)), lens=58)
    turn = bpy.data.objects.new("Turntable", None)
    sc.collection.objects.link(turn)
    for o in list(bpy.data.objects):
        if o is turn: continue
        if o.parent is None and o.type in {'ARMATURE','MESH','EMPTY'}:
            o.parent = turn
            o.matrix_parent_inverse = turn.matrix_world.inverted()
    for i in range(nframes):
        turn.rotation_euler.z = 2*math.pi*i/nframes
        fr = int(round(i/nframes*dur)) if dur>1 else 0
        sc.frame_set(fr)
        sc.render.filepath = f"{prefix}{i:03d}.png"
        bpy.ops.render.render(write_still=True)
    print(f"CEL_DONE={prefix} frames={nframes}")

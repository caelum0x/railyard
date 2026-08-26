import bpy, sys, json, mathutils

argv = sys.argv[sys.argv.index("--")+1:]
src = argv[0]
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=src)

ob = next(o for o in bpy.data.objects if o.type=='MESH')
mw = ob.matrix_world
vs = [mw @ v.co for v in ob.data.vertices]
zs = [v.z for v in vs]
zmin, zmax = min(zs), max(zs)
H = zmax - zmin

# slice into 20 vertical bands, report x-extent, count, x-cluster (bimodal for legs)
bands = 24
out = []
for i in range(bands):
    z0 = zmin + H*i/bands
    z1 = zmin + H*(i+1)/bands
    band = [v for v in vs if z0 <= v.z < z1]
    if not band:
        out.append({"z": round((z0+z1)/2,3), "n":0}); continue
    xs = sorted(v.x for v in band)
    ys = [v.y for v in band]
    # detect gap in x (bimodal -> legs)
    gaps = []
    for a,b in zip(xs, xs[1:]):
        gaps.append((b-a, a, b))
    biggap = max(gaps) if gaps else (0,0,0)
    out.append({
        "z": round((z0+z1)/2,3), "n": len(band),
        "xmin": round(xs[0],3), "xmax": round(xs[-1],3),
        "xspan": round(xs[-1]-xs[0],3),
        "biggap": round(biggap[0],3), "gap_at_x": round((biggap[1]+biggap[2])/2,3),
        "ymid": round(sum(ys)/len(ys),3),
    })
print("H=%.4f zmin=%.4f zmax=%.4f"%(H,zmin,zmax))
print("LM_JSON="+json.dumps(out))

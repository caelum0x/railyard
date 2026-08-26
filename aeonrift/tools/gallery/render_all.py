"""Batch studio-render every champion .glb in the manifest, in ONE headless
Blender process (avoids paying the startup cost 47 times).

Usage:
    blender --background --python render_all.py -- <manifest.json> <repo_root>
"""

import bpy  # noqa: F401  (ensures we are inside Blender)
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import render_champion as rc  # noqa: E402


def argv_after_ddash():
    argv = sys.argv
    return argv[argv.index("--") + 1:] if "--" in argv else []


def main():
    args = argv_after_ddash()
    manifest = args[0]
    root = args[1] if len(args) > 1 else os.getcwd()
    rows = json.load(open(manifest, encoding="utf-8"))
    ok, fail = 0, []
    for i, r in enumerate(rows, 1):
        glb = os.path.join(root, r["glb"])
        out = os.path.join(root, r["png"])
        print("=== [%d/%d] %s ===" % (i, len(rows), r["slug"]))
        try:
            good = rc.render_one(glb, out, r["name"], r["faction"], r["archetype"], r["color"])
        except Exception as e:  # never abort the whole batch on one bad model
            print("GALLERY_ERROR: exception on %s: %r" % (r["slug"], e))
            good = False
        if good and os.path.exists(out):
            ok += 1
        else:
            fail.append(r["slug"])
    print("=== BATCH DONE: %d ok, %d failed ===" % (ok, len(fail)))
    if fail:
        print("FAILED:", ",".join(fail))


if __name__ == "__main__":
    main()

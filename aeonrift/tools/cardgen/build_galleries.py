#!/usr/bin/env python3
"""Build the two self-contained contact sheets for the splash-style exploration.

  GALLERY.html  -- the ANCHOR SET: every __A_flux.png candidate (the locked
                   League-painterly look), grouped by faction, labelled for
                   hand-picking ~18 style-LoRA training images.
  COMPARE.html  -- the STYLE DECISION: for the 5 probe champions, flux (A) vs
                   recraft (B) side by side, documenting why flux was chosen.

Reads manifest.json + the PNGs already on disk. Writes only HTML (headless).
Image srcs are bare filenames so the HTML opens locally from the same folder.
"""
from __future__ import annotations
import json, glob, os
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent.parent / "assets" / "cards" / "style-test"
MANIFEST = OUT / "manifest.json"
MODEL = "black-forest-labs/flux-1.1-pro"
STYLE_SUFFIX = (
    "League of Legends splash art style, dynamic full-body action pose mid-motion, low "
    "dramatic camera angle, cinematic key-art composition, moody dark atmospheric environment, "
    "strong rim lighting with a glowing magical accent color, volumetric fog and floating "
    "particles, painterly semi-realistic digital illustration, ornate highly detailed fantasy "
    "armor and costume, rich cinematic color grade, sense of depth and atmosphere, tells a small "
    "story, epic fantasy concept art, artstation trending, highly detailed, sharp focus"
)
FACTION_ORDER = ["Greek", "Roman", "Egyptian", "Norse", "Tide", "Ember", "Verdant", "Umbral"]
PROBES = ["zeus", "anubis-warden-of-scales", "mars", "nyxralla", "vashka"]

CSS = """
*{box-sizing:border-box}body{margin:0;background:#0b0d12;color:#e7e9ee;
font:14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif}
header{padding:28px 32px;border-bottom:1px solid #1e2230;background:#0e1119;position:sticky;top:0;z-index:5}
h1{margin:0 0 6px;font-size:22px;letter-spacing:.3px}
.sub{color:#98a2b8;font-size:13px;max-width:1100px}
.pill{display:inline-block;padding:2px 9px;border-radius:20px;background:#182033;color:#9fd0ff;
font-size:11px;margin-right:6px}
code{background:#161a26;padding:2px 6px;border-radius:5px;color:#c9d4e8;font-size:12px}
.suffix{margin:14px 0 0;padding:12px 14px;background:#11151f;border:1px solid #1e2434;border-radius:8px;
color:#aeb8cc;font-size:12px;max-width:1100px}
h2{margin:30px 32px 10px;font-size:16px;color:#dfe4ee;border-left:3px solid #3a6ea5;padding-left:10px}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:16px;padding:8px 32px 24px}
.card{background:#11141d;border:1px solid #1c2130;border-radius:10px;overflow:hidden}
.card img{width:100%;display:block;aspect-ratio:4/5;object-fit:cover;background:#05070b}
.meta{padding:8px 11px}
.name{font-weight:600;font-size:13px}
.tags{color:#8a94a8;font-size:11px;margin-top:2px}
.badge{display:inline-block;font-size:10px;padding:1px 7px;border-radius:10px;margin-right:5px}
.cmp{display:grid;grid-template-columns:1fr 1fr;gap:14px;padding:4px 32px 26px}
.cmp .col{background:#11141d;border:1px solid #1c2130;border-radius:10px;overflow:hidden}
.cmp img{width:100%;display:block}
.win{border-color:#3a7d44;box-shadow:0 0 0 1px #3a7d44 inset}
.verdict{margin:16px 32px;padding:14px 16px;background:#0f1a12;border:1px solid #2c5236;border-radius:8px;color:#bfe6c8}
.hd{padding:8px 11px;font-size:12px;color:#aeb8cc;border-bottom:1px solid #1c2130}
.prompt{padding:8px 11px;color:#7f8aa0;font-size:11px}
.missing{padding:40px;text-align:center;color:#5b6577}
"""

FAC_COLOR = {"Greek":"#caa94a","Roman":"#c0553f","Egyptian":"#2ec6c0","Norse":"#5aa9d6",
             "Tide":"#3ec6e0","Ember":"#e07b2e","Verdant":"#4fb85a","Umbral":"#9a6bd0"}

def load():
    return json.loads(MANIFEST.read_text()) if MANIFEST.exists() else {}

def esc(s): return (s or "").replace("&","&amp;").replace("<","&lt;").replace(">","&gt;")

def build_gallery(m):
    # collect flux anchors that have a file on disk
    items = []
    for slug, ap in m.items():
        rec = ap.get("A_flux", {})
        f = rec.get("file")
        if f and (OUT / f).exists():
            items.append((slug, rec))
    n = len(items)
    by_fac = {}
    for slug, rec in items:
        by_fac.setdefault(rec.get("faction", "?"), []).append((slug, rec))
    parts = [f"<!doctype html><meta charset=utf-8><title>Aeonrift - Splash-Style Anchor Set</title><style>{CSS}</style>"]
    parts.append("<header><h1>Aeonrift - League-Painterly Anchor Set (style-LoRA candidates)</h1>")
    parts.append(f"<div class=sub><span class=pill>{n} candidates</span>"
                 f"<span class=pill>model: {MODEL}</span>"
                 f"<span class=pill>4:5 splash</span>"
                 "One converged look held constant across the roster: dramatic dynamic pose, moody "
                 "cinematic rim-light, atmospheric magical FX, ornate detail, painterly semi-realistic. "
                 "Hand-pick the best ~18 as LoRA training images (aim for a spread across factions/archetypes).</div>")
    parts.append(f"<div class=suffix><b>Shared style suffix</b> (identical for every image): {esc(STYLE_SUFFIX)}</div></header>")
    if n == 0:
        parts.append("<div class=missing>No images generated yet.</div>")
    for fac in FACTION_ORDER + [k for k in by_fac if k not in FACTION_ORDER]:
        if fac not in by_fac: continue
        parts.append(f"<h2>{esc(fac)} <span style='color:#6b7488;font-weight:400'>({len(by_fac[fac])})</span></h2><div class=grid>")
        for slug, rec in sorted(by_fac[fac]):
            col = FAC_COLOR.get(fac, "#888")
            parts.append(
                f"<div class=card><a href='{esc(rec['file'])}' target=_blank><img loading=lazy src='{esc(rec['file'])}'></a>"
                f"<div class=meta><div class=name>{esc(rec.get('display', slug))}</div>"
                f"<div class=tags><span class=badge style='background:{col}22;color:{col}'>{esc(fac)}</span>"
                f"{esc(rec.get('archetype',''))} &middot; <code>{esc(slug)}</code></div></div></div>")
        parts.append("</div>")
    (OUT / "GALLERY.html").write_text("".join(parts))
    return n

def build_compare(m):
    parts = [f"<!doctype html><meta charset=utf-8><title>Aeonrift - Style Approach Comparison</title><style>{CSS}</style>"]
    parts.append("<header><h1>Aeonrift - Style Approach Comparison (why flux-1.1-pro won)</h1>"
                 "<div class=sub>Probe champions rendered two ways with the same dynamic-splash prompt. "
                 "<b>Approach A = black-forest-labs/flux-1.1-pro</b> (chosen anchor look). "
                 "<b>Approach B = recraft-ai/recraft-v3</b> (painterly illustration model).</div></header>")
    parts.append("<div class=verdict><b>Verdict:</b> flux-1.1-pro (A) is the anchor. It renders more cinematic and "
                 "semi-realistic with stronger rim-light and atmospheric FX integration; recraft-v3 (B) is a solid "
                 "painterly option but reads flatter / more storybook and stamps fake artist signatures (an AI tell "
                 "and noise for a clean LoRA set). Approach C (SDXL w/ hard negatives) was dropped: that community "
                 "model needs a version-pinned prediction call and added no advantage over flux.</div>")
    for slug in PROBES:
        ap = m.get(slug, {})
        a = ap.get("A_flux", {}); b = ap.get("B_recraft", {})
        parts.append(f"<h2>{esc(a.get('display', slug))} <span style='color:#6b7488;font-weight:400'>"
                     f"({esc(a.get('faction',''))} / {esc(a.get('archetype',''))})</span></h2><div class=cmp>")
        for label, rec, win in [("A - flux-1.1-pro", a, True), ("B - recraft-v3", b, False)]:
            f = rec.get("file")
            cls = "col win" if win else "col"
            if f and (OUT / f).exists():
                parts.append(f"<div class='{cls}'><div class=hd><b>{label}</b>{' (chosen)' if win else ''}</div>"
                             f"<a href='{esc(f)}' target=_blank><img loading=lazy src='{esc(f)}'></a>"
                             f"<div class=prompt>{esc(rec.get('prompt',''))[:240]}...</div></div>")
            else:
                parts.append(f"<div class='{cls}'><div class=hd><b>{label}</b></div><div class=missing>not generated</div></div>")
        parts.append("</div>")
    (OUT / "COMPARE.html").write_text("".join(parts))

def main():
    m = load()
    n = build_gallery(m)
    build_compare(m)
    print(f"GALLERY.html: {n} anchor candidates")
    print(f"COMPARE.html: {len(PROBES)} probe champions")
    print("wrote", OUT / "GALLERY.html")
    print("wrote", OUT / "COMPARE.html")

if __name__ == "__main__":
    main()

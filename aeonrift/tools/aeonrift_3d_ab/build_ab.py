"""Build AB.html from results.json — one row per champion comparing generators.

Columns per champion: concept ref | TRELLIS | Hunyuan3D-2.1 | Hunyuan3D-2mv | (fal...)
Each cell labeled with generator, cost, and triangle count.
"""
from __future__ import annotations

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results.json"
OUT_HTML = HERE / "AB.html"

# generator display order
ORDER = ["trellis", "hunyuan21", "hunyuan2mv", "tripo3d", "meshy6"]


def fmt_tris(n) -> str:
    if not n:
        return "n/a"
    return f"{n:,} tris"


def fmt_cost(c) -> str:
    if c is None:
        return ""
    if c == 0:
        return "$0.00 (reused)"
    return f"${c:.2f}/run"


def cell(disp: str, gen: str, info: dict, label: str) -> str:
    if not info or not info.get("ok"):
        err = (info or {}).get("error", "not run")
        return (f'<td class="miss"><div class="gen">{label}</div>'
                f'<div class="err">FAILED / skipped<br><small>{err}</small></div></td>')
    png = f"out/{disp}/{info['png']}"
    mb = info.get("bytes", 0) / 1e6
    pt = info.get("predict_time")
    pt_s = f" &middot; {pt:.0f}s gen" if pt else ""
    note = info.get("note", "")
    note_s = f" &middot; {note}" if note else ""
    return (
        f'<td>'
        f'<div class="gen">{label}</div>'
        f'<a href="{png}" target="_blank"><img src="{png}" alt="{label}"></a>'
        f'<div class="meta">{fmt_tris(info.get("tris"))} &middot; {mb:.1f} MB</div>'
        f'<div class="meta cost">{fmt_cost(info.get("cost"))}{pt_s}{note_s}</div>'
        f'</td>'
    )


def ref_cell(disp: str, champ: dict) -> str:
    png = f"out/{disp}/{champ['ref']}"
    return (f'<td class="ref"><div class="gen">Concept ref (Flux)</div>'
            f'<a href="{png}" target="_blank"><img src="{png}" alt="ref"></a>'
            f'<div class="meta">image-to-3D input</div></td>')


def main() -> int:
    data = json.loads(RESULTS.read_text())
    champs = data["champions"]
    labels = data["gen_label"]
    fal = data.get("fal_available", False)

    # which generators actually appear in any champion (in ORDER)
    present = [g for g in ORDER if any(g in c["generators"] for c in champs.values())]

    rows = []
    for disp, champ in champs.items():
        cells = [f'<td class="name"><h2>{disp.title()}</h2><div class="slug">{champ["slug"]}</div></td>']
        cells.append(ref_cell(disp, champ))
        for g in present:
            cells.append(cell(disp, g, champ["generators"].get(g), labels.get(g, g)))
        rows.append("<tr>" + "".join(cells) + "</tr>")

    header_cols = ['<th>Champion</th>', '<th>Concept ref</th>'] + [f'<th>{labels.get(g,g)}</th>' for g in present]

    fal_note = ("fal generators (Tripo v3, Meshy v6) were <b>skipped</b> — no FAL key found in the environment."
                if not fal else "fal key present.")

    html = f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Aeonrift image-to-3D generator A/B — Anubis &amp; Zeus</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background:#0d0e12; color:#e7e9ee; margin:0; padding:24px; }}
  h1 {{ font-size:22px; margin:0 0 4px; }}
  .sub {{ color:#9aa0ad; font-size:13px; margin-bottom:20px; }}
  table {{ border-collapse:separate; border-spacing:10px; width:100%; }}
  th {{ font-size:12px; text-transform:uppercase; letter-spacing:.04em; color:#9aa0ad;
    font-weight:600; text-align:left; padding:0 6px; }}
  td {{ background:#16181f; border:1px solid #23262f; border-radius:12px; padding:10px;
    vertical-align:top; text-align:center; width:260px; }}
  td.name {{ background:transparent; border:none; text-align:left; width:120px; vertical-align:middle; }}
  td.name h2 {{ margin:0; font-size:19px; }}
  td.name .slug {{ color:#6b7280; font-size:11px; }}
  td.ref {{ background:#12141a; }}
  img {{ width:100%; max-width:240px; height:auto; border-radius:8px; display:block;
    margin:6px auto; background:#000; }}
  .gen {{ font-size:12px; font-weight:600; color:#c7ccd6; margin-bottom:4px; min-height:30px; }}
  .meta {{ font-size:11px; color:#9aa0ad; margin-top:4px; }}
  .cost {{ color:#7fd1a6; }}
  .miss .err {{ color:#e07a7a; font-size:12px; padding:40px 6px; }}
  .legend {{ margin-top:24px; font-size:12px; color:#9aa0ad; line-height:1.6; }}
  .legend b {{ color:#c7ccd6; }}
  code {{ background:#23262f; padding:1px 5px; border-radius:4px; }}
</style></head><body>
<h1>Aeonrift image-to-3D generator A/B</h1>
<div class="sub">Two champions (Anubis, Zeus) &mdash; same Flux concept reference into each generator, then
rendered with an identical headless Blender EEVEE camera/light rig. {fal_note}</div>
<table>
<thead><tr>{''.join(header_cols)}</tr></thead>
<tbody>
{chr(10).join(rows)}
</tbody>
</table>
<div class="legend">
<b>How to read:</b> every model was fed the <i>same</i> <code>reference.png</code> and rendered with the
same camera and 3-point lighting, so differences are the generator's, not the render's.
Tri-count is measured post-import in Blender (triangulated). Cost is the per-run Replicate estimate;
gen time is the model's <code>predict_time</code>. TRELLIS is the current baseline (reused, no new spend).<br>
<b>Generators:</b> TRELLIS <code>firtoz/trellis</code> &middot; Hunyuan3D-2.1 <code>ndreca/hunyuan3d-2.1</code>
(PBR texture + auto-rig) &middot; Hunyuan3D-2mv <code>tencent/hunyuan3d-2mv</code> (single front view).
</div>
<div class="legend" style="margin-top:20px;border-top:1px solid #23262f;padding-top:16px;">
<b style="color:#7fd1a6;font-size:14px;">Verdict &mdash; winner: Hunyuan3D-2.1</b><br>
<b>Hunyuan3D-2.1</b> is clearly the best stylized game-ready result on both champions: sharpest
geometry, faithful PBR texture, and it preserves flowing/ornate detail the baseline loses
(e.g. Zeus's full flowing cape and layered gold armor; Anubis's crisp collar, kilt glyphs and
fluffy tail). ~$0.27/run, ~110&ndash;125s. Minor artifacts to QA: occasional glitch shard where the
concept holds a prop (Zeus's lightning bolt) and slight face-texture bleed.<br>
<b>TRELLIS (baseline)</b> is acceptable and cheap but noticeably softer/&ldquo;melted&rdquo; &mdash; blobby hands,
truncated Zeus cape, less crisp ornamentation.<br>
<b>Hunyuan3D-2mv</b> produced clean geometry but <b>no texture</b> (white clay) from a single front
image &mdash; not usable as-is without true multi-view inputs or a separate texture pass, despite being
cheapest/fastest ($0.11, ~12&ndash;17s).<br>
<b>Recommendation:</b> switch the full 47-batch to Hunyuan3D-2.1 (~47 &times; $0.27 &asymp; $12.70 total),
with a light artifact-QA pass. Keep TRELLIS only as a cheap fallback; skip 2mv unless fed multi-view refs.
</div>
</body></html>
"""
    OUT_HTML.write_text(html)
    print(f"WROTE {OUT_HTML}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

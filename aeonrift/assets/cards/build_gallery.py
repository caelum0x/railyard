"""Build assets/cards/GALLERY.html -- one file showing all final champion cards.

Fully headless: writes an HTML file with relative <img> paths into the final/
and art/ folders. Cards are grouped by faction; a few raw Flux illustrations are
shown at the bottom.
"""
from __future__ import annotations

from pathlib import Path

from champions import load_all, FACTION_ORDER, FACTION_COLOR

HERE = Path(__file__).resolve().parent
FINAL = HERE / "final"
ART = HERE / "art"
OUT = HERE / "GALLERY.html"


def main() -> int:
    champs = load_all()
    by_faction: dict[str, list] = {f: [] for f in FACTION_ORDER}
    for c in champs:
        if (FINAL / f"{c.slug}.png").exists():
            by_faction.setdefault(c.faction, []).append(c)

    total = sum(len(v) for v in by_faction.values())

    parts = [
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width, initial-scale=1'>",
        "<title>Aeonrift — Champion Card Gallery</title>",
        "<style>",
        ":root{color-scheme:dark;}",
        "body{margin:0;background:radial-gradient(1200px 700px at 50% -5%,#20202c,#0c0c11 60%);",
        "color:#e8e8f0;font-family:'Avenir Next','Helvetica Neue',system-ui,sans-serif;}",
        ".wrap{max-width:1440px;margin:0 auto;padding:40px 28px 80px;}",
        "h1{font-family:Copperplate,'Optima',serif;letter-spacing:3px;font-size:40px;",
        "margin:0 0 4px;text-align:center;}",
        ".sub{text-align:center;color:#9a9aad;margin:0 0 40px;font-size:15px;letter-spacing:1px;}",
        ".faction{margin:44px 0 14px;display:flex;align-items:center;gap:14px;}",
        ".faction h2{font-family:Copperplate,serif;letter-spacing:2px;font-size:22px;margin:0;}",
        ".faction .rule{flex:1;height:1px;background:linear-gradient(90deg,currentColor,transparent);opacity:.5;}",
        ".grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(230px,1fr));gap:22px;}",
        ".card{border-radius:16px;overflow:hidden;transition:transform .15s ease;",
        "box-shadow:0 10px 30px rgba(0,0,0,.5);}",
        ".card:hover{transform:translateY(-6px) scale(1.02);}",
        ".card img{width:100%;display:block;}",
        ".raw h2{font-family:Copperplate,serif;letter-spacing:2px;}",
        ".rawgrid{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:18px;}",
        ".rawgrid img{width:100%;border-radius:12px;display:block;box-shadow:0 8px 22px rgba(0,0,0,.5);}",
        "</style></head><body><div class='wrap'>",
        "<h1>AEONRIFT</h1>",
        f"<p class='sub'>Champion Card Set · {total} cards · real Flux AI key art · grouped by faction</p>",
    ]

    order = [f for f in FACTION_ORDER if by_faction.get(f)]
    for f in by_faction:
        if f not in order and by_faction[f]:
            order.append(f)
    for f in order:
        cards = by_faction.get(f) or []
        if not cards:
            continue
        accent = FACTION_COLOR.get(f, ("#888888", ""))[0]
        parts.append(f"<div class='faction' style='color:{accent}'>")
        parts.append(f"<h2>{f}</h2><div class='rule'></div>"
                     f"<span style='color:#8a8a9a;font-size:13px'>{len(cards)}</span></div>")
        parts.append("<div class='grid'>")
        for c in sorted(cards, key=lambda x: (-x.cost, x.name)):
            parts.append(
                f"<div class='card'><img loading='lazy' src='final/{c.slug}.png' "
                f"alt='{c.name}'></div>"
            )
        parts.append("</div>")

    # A few raw illustrations.
    raw = [c for c in champs if (ART / f"{c.slug}.png").exists()][:6]
    if raw:
        parts.append("<div class='raw'><div class='faction' style='color:#c9a24a'>"
                     "<h2>Raw Flux Illustrations</h2><div class='rule'></div></div>")
        parts.append("<div class='rawgrid'>")
        for c in raw:
            parts.append(f"<img loading='lazy' src='art/{c.slug}.png' alt='{c.name} art'>")
        parts.append("</div></div>")

    parts.append("</div></body></html>")
    OUT.write_text("\n".join(parts))
    print(f"gallery: {OUT}  ({total} cards)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

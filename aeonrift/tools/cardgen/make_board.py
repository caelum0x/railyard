#!/usr/bin/env python3
"""Generate board_mockup.html — a headless reference of the Ascendancy match UI.

Writes a single self-contained HTML file (all CSS + SVG inlined, zero external
requests) showing what a live match should look like: two lanes, tower/base HP,
an elixir bar, deployed unit tokens, and a hand of four REAL generated champion
cards. It is a design reference you open in a browser to view — this script only
writes the file, it never opens anything.

    python3 tools/cardgen/make_board.py     # -> tools/cardgen/board_mockup.html
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from ron_parse import load_champion, parse_roster_meta  # noqa: E402

AEON = HERE.parent.parent
CARDS = AEON / "assets" / "cards"
CHAMP_DIR = AEON / "content" / "champions"
OUT = HERE / "board_mockup.html"

# A deliberately diverse hand so the mockup shows the palette range.
HAND = ["achilles", "ysmera", "loki", "brackmoor"]
# Units currently "deployed" on the board (slug, side, lane-x%, lane-y%).
UNITS = [
    ("dregmaw", "enemy", 30, 26), ("medusa", "enemy", 66, 20),
    ("thor", "ally", 38, 62), ("cyranth", "ally", 70, 70),
]


def inline_card(slug: str, width: int = 150) -> str:
    svg = (CARDS / f"{slug}.svg").read_text(encoding="utf-8")
    # scale by replacing the root width/height with a CSS-friendly size
    svg = re.sub(r'width="\d+" height="\d+"', f'width="{width}"', svg, count=1)
    return svg


def token(slug: str, side: str, x: int, y: int, roster: dict) -> str:
    m = load_champion(slug, CHAMP_DIR, roster)
    from factions import faction_tokens
    fac = faction_tokens(m["faction"])
    ring = "#4ad2ff" if side == "ally" else "#ff5a6a"
    letter = m["name"][:1].upper()
    hpw = 34
    return f'''<div class="token {side}" style="left:{x}%;top:{y}%">
      <div class="hpbar"><i style="width:{hpw}px;background:{ring}"></i></div>
      <div class="disc" style="background:radial-gradient(circle at 35% 30%,{fac['sigil0']},{fac['sigil1']});border-color:{ring}">
        <span>{letter}</span>
      </div>
      <div class="tname">{m['name']}</div>
    </div>'''


def elixir_pips(filled: int, total: int = 10) -> str:
    pips = "".join(
        f'<i class="{"on" if i < filled else ""}"></i>' for i in range(total)
    )
    return f'<div class="elixir"><b>{filled}</b><div class="pips">{pips}</div></div>'


def hand_card(slug: str, cost: int, ready: bool) -> str:
    cls = "hcard" + ("" if ready else " locked")
    lock = "" if ready else '<div class="veil"></div>'
    return f'''<div class="{cls}">
      <div class="cost">{cost}</div>{inline_card(slug)}{lock}
    </div>'''


def main() -> int:
    roster = parse_roster_meta((CHAMP_DIR / "roster_meta.ron").read_text(encoding="utf-8"))
    tokens = "".join(token(s, side, x, y, roster) for s, side, x, y in UNITS)

    hand_models = {s: load_champion(s, CHAMP_DIR, roster) for s in HAND}
    elixir = 7
    hand = "".join(
        hand_card(s, hand_models[s]["cost"], hand_models[s]["cost"] <= elixir)
        for s in HAND
    )

    html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Aeonrift · Ascendancy — Match Screen (mockup)</title>
<style>
  :root{{--bg:#0a0812;--edge:#2a2140;--ally:#4ad2ff;--enemy:#ff5a6a;--gold:#f5c542}}
  *{{box-sizing:border-box}}
  body{{margin:0;background:radial-gradient(120% 80% at 50% -10%,#1a1430,#070510);
    color:#eae3fb;font-family:system-ui,-apple-system,Segoe UI,sans-serif;
    min-height:100vh;display:flex;justify-content:center;padding:18px}}
  .phone{{width:430px;max-width:100%;background:linear-gradient(#0d0a18,#0a0812);
    border:1px solid var(--edge);border-radius:26px;overflow:hidden;
    box-shadow:0 30px 80px rgba(0,0,0,.6);display:flex;flex-direction:column}}
  /* --- top HUD: enemy base --- */
  .hud{{display:flex;justify-content:space-between;align-items:center;
    padding:10px 16px;background:linear-gradient(#1a0e16,#12060c);
    border-bottom:1px solid #3a1a24}}
  .base{{display:flex;align-items:center;gap:8px;font-size:12px;font-weight:700}}
  .crown{{width:22px;height:22px;border-radius:50%;display:grid;place-items:center;
    font-size:12px;background:#2a1420;border:1px solid #6a2a38;color:var(--enemy)}}
  .basehp{{width:120px;height:9px;border-radius:6px;background:#2a1018;overflow:hidden}}
  .basehp i{{display:block;height:100%;background:linear-gradient(90deg,#ff8a5a,#ff3a5a)}}
  .basehp.ally i{{background:linear-gradient(90deg,#4affce,#2ba6ff)}}
  /* --- arena --- */
  .arena{{position:relative;flex:1;min-height:430px;
    background:
      linear-gradient(180deg,rgba(255,60,90,.10),rgba(0,0,0,0) 34%,rgba(0,0,0,0) 66%,rgba(74,210,255,.10)),
      repeating-linear-gradient(0deg,#0e0b1a,#0e0b1a 2px,#0c0916 2px,#0c0916 40px),
      #0b0814;
    border-top:1px solid #241a3a;border-bottom:1px solid #241a3a;overflow:hidden}}
  .river{{position:absolute;left:0;right:0;top:calc(50% - 14px);height:28px;
    background:linear-gradient(90deg,rgba(43,188,207,.35),rgba(122,75,196,.35));
    border-top:1px solid rgba(140,220,255,.4);border-bottom:1px solid rgba(140,220,255,.4)}}
  .lane{{position:absolute;top:0;bottom:0;width:1px;background:rgba(255,255,255,.06)}}
  .lane.l{{left:33%}} .lane.r{{left:67%}}
  .tower{{position:absolute;width:46px;height:54px;border-radius:8px 8px 4px 4px;
    background:linear-gradient(#2a2340,#171226);border:1px solid #4a3f6e;
    display:grid;place-items:center;font-size:10px;color:#b9aee6;transform:translate(-50%,-50%)}}
  .tower.enemy{{border-color:#6a2a38}} .tower.ally{{border-color:#2a5a6a}}
  .tower b{{font-size:14px;color:var(--gold)}}
  .token{{position:absolute;transform:translate(-50%,-50%);text-align:center;width:60px}}
  .disc{{width:46px;height:46px;border-radius:50%;margin:0 auto;border:3px solid;
    display:grid;place-items:center;box-shadow:0 6px 14px rgba(0,0,0,.5)}}
  .disc span{{font:700 22px Georgia,serif;color:#fff;text-shadow:0 1px 2px #000}}
  .token.enemy .disc{{box-shadow:0 0 0 2px rgba(255,90,106,.35),0 6px 14px rgba(0,0,0,.5)}}
  .token.ally  .disc{{box-shadow:0 0 0 2px rgba(74,210,255,.35),0 6px 14px rgba(0,0,0,.5)}}
  .hpbar{{width:38px;height:5px;margin:0 auto 4px;background:#000a;border-radius:3px;overflow:hidden}}
  .hpbar i{{display:block;height:100%}}
  .tname{{font-size:9px;margin-top:3px;color:#cfc6ea;text-shadow:0 1px 2px #000}}
  /* --- elixir --- */
  .elixir{{display:flex;align-items:center;gap:10px;padding:8px 16px;
    background:linear-gradient(#160e24,#0f0a1a);border-top:1px solid #2a2140}}
  .elixir>b{{font:700 18px Georgia,serif;color:#e06bff;width:22px}}
  .pips{{display:flex;gap:4px;flex:1}}
  .pips i{{flex:1;height:14px;border-radius:4px;background:#241a34;
    border:1px solid #3a2a52}}
  .pips i.on{{background:linear-gradient(#e06bff,#a02fe0);border-color:#c04bff;
    box-shadow:0 0 8px rgba(224,107,255,.6)}}
  /* --- hand --- */
  .hand{{display:grid;grid-template-columns:repeat(4,1fr);gap:8px;padding:12px 12px 16px;
    background:linear-gradient(#120d1e,#0a0712)}}
  .hcard{{position:relative;border-radius:12px;overflow:hidden;
    transition:transform .12s;cursor:pointer}}
  .hcard svg{{width:100%;display:block}}
  .hcard:hover{{transform:translateY(-6px)}}
  .hcard .cost{{position:absolute;top:6px;left:6px;z-index:3;width:26px;height:26px;
    border-radius:50%;background:radial-gradient(circle at 35% 30%,#e06bff,#7d1fb0);
    display:grid;place-items:center;font:700 14px Georgia,serif;color:#fff;
    border:2px solid #fff8;box-shadow:0 2px 6px #000a}}
  .hcard.locked{{filter:saturate(.5)}}
  .veil{{position:absolute;inset:0;background:rgba(6,4,12,.55);z-index:2}}
  .cap{{text-align:center;font-size:11px;color:#8a7fb0;padding:0 0 14px;letter-spacing:1px}}
  .tag{{text-align:center;font-size:11px;color:#9a90b8;padding:10px 0 2px;letter-spacing:2px}}
</style></head>
<body><div class="phone">

  <div class="tag">AEONRIFT · ASCENDANCY — MATCH MOCKUP</div>

  <div class="hud">
    <div class="base"><span class="crown">♛</span> ENEMY BASE
      <div class="basehp"><i style="width:72%"></i></div></div>
    <div class="base">YOUR BASE
      <div class="basehp ally"><i style="width:88%"></i></div>
      <span class="crown" style="color:var(--ally);border-color:#2a5a6a">♛</span></div>
  </div>

  <div class="arena">
    <div class="lane l"></div><div class="lane r"></div>
    <div class="river"></div>
    <div class="tower enemy" style="left:33%;top:16%"><span>TOWER</span><b>1.4k</b></div>
    <div class="tower enemy" style="left:67%;top:16%"><span>TOWER</span><b>2.1k</b></div>
    <div class="tower ally"  style="left:33%;top:84%"><span>TOWER</span><b>2.6k</b></div>
    <div class="tower ally"  style="left:67%;top:84%"><span>TOWER</span><b>2.6k</b></div>
    {tokens}
  </div>

  {elixir_pips(elixir)}

  <div class="hand">{hand}</div>
  <div class="cap">Tap a card, then a lane, to deploy · 4-card hand · procedural card art</div>

</div></body></html>"""

    OUT.write_text(html, encoding="utf-8")
    print(f"Wrote {OUT.relative_to(AEON)} ({len(html)//1024} KB, self-contained)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

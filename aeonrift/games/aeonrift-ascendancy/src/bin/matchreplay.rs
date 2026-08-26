//! `matchreplay` — the headless match-replay gallery generator.
//!
//! Runs a scripted, seeded, deterministic bot-vs-bot match through the SAME rules core
//! the game uses, samples the board every few ticks (via `rules::replay`), and writes a
//! set of HTML artifacts that render each board state with the REAL finished card faces
//! (`assets/cards/final-v2/*.png`) — so the match can be *seen playing* with no window
//! and no screen capture:
//!
//!   - `REPLAY.html`   — an interactive, self-driving replay (play / step / scrub).
//!   - `BOARD.html`    — a single static mobile board/HUD mockup (the busiest beat).
//!   - `turn_00..NN.html` — a handful of standalone board frames to spot-check.
//!
//! Usage:
//!   cargo run -p aeonrift-ascendancy --bin matchreplay -- \
//!       [--seed N] [--interval N] [--snap N] [--deck0 NAME] [--deck1 NAME] [--out DIR]
//!
//! Everything here is presentation/IO; the deterministic recording lives in the tested
//! `rules::replay` module.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cards::deck::preset_decks::load_deck_file;
use cards::rules::replay::{record_match, MatchReplay, SlugCard, Snapshot};
use cards::{MatchConfig, Outcome, CARD_FACES_DIR, DEFAULT_DEPLOY_INTERVAL};

const DECKS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/cards/decks");
const CHAMPS_DIR: &str = cards::CONTENT_CHAMPIONS_DIR;

/// A hand entry for the HUD strip (player 0's deck).
struct HandCard {
    slug: String,
    name: String,
    cost: u32,
}

fn load_deck(name: &str) -> Result<Vec<SlugCard>, String> {
    let path = format!("{DECKS_DIR}/{name}.ron");
    let preset = load_deck_file(&path, CHAMPS_DIR)
        .map_err(|e| format!("failed to load deck '{name}' ({path}): {e}"))?;
    Ok(preset
        .slugs
        .into_iter()
        .zip(preset.cards)
        .map(|(slug, card)| SlugCard::new(slug, card))
        .collect())
}

/// A minimal relative path from `from_dir` to `to` (both absolute, sharing a repo
/// ancestor). Keeps the emitted HTML self-contained without copying the big PNGs.
fn rel_path(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let mut i = 0;
    while i < from.len() && i < to_c.len() && from[i] == to_c[i] {
        i += 1;
    }
    let ups = std::iter::repeat("..").take(from.len() - i);
    let downs = to_c[i..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    ups.map(str::to_string)
        .chain(downs)
        .collect::<Vec<_>>()
        .join("/")
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Serialise the recorded snapshots to a compact JS array literal (no serde dep).
fn snapshots_to_js(snaps: &[Snapshot]) -> String {
    let mut out = String::from("[");
    for (i, s) in snaps.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let outcome = match s.outcome {
            None => "null".to_string(),
            Some(Outcome::Winner(p)) => format!("{p}"),
            Some(Outcome::Draw) => "\"draw\"".to_string(),
        };
        out.push_str(&format!(
            "{{t:{},e:[{},{}],b:[{},{}],bmax:{},o:{},u:[",
            s.tick, s.elixir[0], s.elixir[1], s.base_hp[0], s.base_hp[1], s.base_hp_max, outcome
        ));
        for (j, u) in s.units.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{tm:{},ln:{},s:\"{}\",n:\"{}\",f:{:.3},p:{:.3}}}",
                u.team,
                u.lane,
                esc(&u.slug),
                esc(&u.name),
                u.hp_frac,
                u.progress
            ));
        }
        out.push_str("]}");
    }
    out.push(']');
    out
}

fn hand_to_js(hand: &[HandCard]) -> String {
    let mut out = String::from("[");
    for (i, h) in hand.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{s:\"{}\",n:\"{}\",c:{}}}",
            esc(&h.slug),
            esc(&h.name),
            h.cost
        ));
    }
    out.push(']');
    out
}

/// The shared CSS + board-drawing JS used by both the interactive replay and the
/// static frames. `mount(state, hand, faces, lanes)` renders one board into `#app`.
fn board_script(faces_rel: &str, lanes: usize) -> String {
    format!(
        r#"const FACES="{faces_rel}";const LANES={lanes};
function face(slug){{return FACES+"/"+slug+".png";}}
function drawBoard(app,s,hand,title){{
  const p0=s.b[0],p1=s.b[1],bmax=s.bmax;
  const winTxt = s.o===null?"":(s.o==="draw"?"DRAW":("PLAYER "+s.o+" WINS"));
  let lanesHtml="";
  for(let ln=0;ln<LANES;ln++){{lanesHtml+=`<div class="lane"></div>`;}}
  let unitsHtml="";
  for(const u of s.u){{
    const col=(u.ln+0.5)/LANES*100;
    const top=u.tm===0?(100-u.p*100):(u.p*100);
    const cls=u.tm===0?"u0":"u1";
    unitsHtml+=`<div class="unit ${{cls}}" style="left:${{col}}%;top:${{top}}%">
      <img src="${{face(u.s)}}" alt="${{u.n}}"/>
      <div class="uhp"><i style="width:${{Math.max(0,u.f*100)}}%"></i></div></div>`;
  }}
  let handHtml="";
  for(const h of hand){{
    handHtml+=`<div class="hand-card"><img src="${{face(h.s)}}" alt="${{h.n}}"/>
      <span class="cost">${{h.c}}</span></div>`;
  }}
  app.innerHTML=`
   <div class="phone">
    <div class="hud-top">
      <div class="pill enemy"><b>ENEMY</b><div class="hpbar"><i style="width:${{Math.max(0,p1/bmax*100)}}%"></i></div><span>${{Math.max(0,p1)}}</span></div>
      <div class="turn">${{title}}<br><small>tick ${{s.t}}</small></div>
    </div>
    <div class="field">
      <div class="base base1">ENEMY NEXUS</div>
      ${{lanesHtml}}${{unitsHtml}}
      <div class="base base0">YOUR NEXUS</div>
      ${{winTxt?`<div class="banner">${{winTxt}}</div>`:""}}
    </div>
    <div class="hud-bot">
      <div class="pill you"><b>YOU</b><div class="hpbar"><i style="width:${{Math.max(0,p0/bmax*100)}}%"></i></div><span>${{Math.max(0,p0)}}</span></div>
      <div class="elixir"><span>ELIXIR</span><div class="ebar"><i style="width:${{s.e[0]*10}}%"></i></div><b>${{s.e[0]}}/10</b></div>
      <div class="hand">${{handHtml}}</div>
    </div>
   </div>`;
}}"#
    )
}

const STYLE: &str = r#"
:root{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:#07060c;color:#ece7f6;font-family:system-ui,Segoe UI,sans-serif;
  display:flex;flex-direction:column;align-items:center;gap:14px;padding:18px}
h1{font-family:Georgia,serif;letter-spacing:4px;font-size:20px;margin:6px 0 0}
.sub{color:#9a8fbe;font-size:12px;letter-spacing:1px;margin:0}
.phone{width:390px;height:760px;background:linear-gradient(180deg,#140d22,#0a0712);
  border:1px solid #2a2140;border-radius:30px;overflow:hidden;display:flex;flex-direction:column;
  box-shadow:0 24px 70px rgba(0,0,0,.6)}
.hud-top,.hud-bot{padding:10px 14px;display:flex;flex-direction:column;gap:8px;background:rgba(0,0,0,.25)}
.hud-top{flex-direction:row;align-items:center;justify-content:space-between}
.pill{display:flex;align-items:center;gap:8px;font-size:11px;letter-spacing:1px}
.pill b{min-width:44px}
.hpbar{width:120px;height:9px;background:#241c38;border-radius:5px;overflow:hidden}
.hpbar i{display:block;height:100%}
.pill.enemy .hpbar i{background:linear-gradient(90deg,#ff5a5a,#ffb454)}
.pill.you .hpbar i{background:linear-gradient(90deg,#5ad1ff,#8affc0)}
.pill span{font-variant-numeric:tabular-nums;color:#cfc6ea}
.turn{text-align:right;font-size:12px;letter-spacing:2px;color:#d7cbff}
.turn small{color:#8a7fae;letter-spacing:1px}
.field{position:relative;flex:1;margin:6px 12px;border:1px solid #241c38;border-radius:16px;
  background:radial-gradient(120% 60% at 50% 0%,#1a1030,#0c0818 70%);overflow:hidden}
.lane{position:absolute;top:0;bottom:0;width:1px;background:#2a2142}
.lane:nth-child(1){left:33.3%}.lane:nth-child(2){left:66.6%}
.base{position:absolute;left:8%;right:8%;height:34px;border-radius:10px;display:flex;
  align-items:center;justify-content:center;font-size:11px;letter-spacing:2px;font-weight:700}
.base1{top:8px;background:linear-gradient(180deg,#5a1e28,#3a1420);color:#ffd7d7;border:1px solid #7a2b38}
.base0{bottom:8px;background:linear-gradient(180deg,#123b48,#0c2630);color:#d3f2ff;border:1px solid #23606f}
.unit{position:absolute;width:46px;height:60px;transform:translate(-50%,-50%);transition:top .25s linear,left .25s linear}
.unit img{width:100%;height:100%;object-fit:cover;border-radius:8px;display:block;
  box-shadow:0 4px 12px rgba(0,0,0,.6)}
.unit.u0 img{border:2px solid #4fd1ff}.unit.u1 img{border:2px solid #ff6a6a}
.uhp{height:4px;background:#20182e;border-radius:2px;margin-top:2px;overflow:hidden}
.uhp i{display:block;height:100%;background:#8affc0}
.unit.u1 .uhp i{background:#ffb0b0}
.banner{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;
  font-family:Georgia,serif;font-size:26px;letter-spacing:3px;color:#fff2c2;
  background:rgba(6,4,12,.55);text-shadow:0 2px 20px #000}
.elixir{display:flex;align-items:center;gap:8px;font-size:10px;letter-spacing:1px;color:#e0b3ff}
.ebar{flex:1;height:10px;background:#241c38;border-radius:5px;overflow:hidden}
.ebar i{display:block;height:100%;background:linear-gradient(90deg,#c06bff,#ff7ae0)}
.hand{display:flex;gap:7px;justify-content:center}
.hand-card{position:relative;width:60px;height:80px}
.hand-card img{width:100%;height:100%;object-fit:cover;border-radius:9px;border:1px solid #3a2f55;
  box-shadow:0 5px 14px rgba(0,0,0,.55)}
.hand-card .cost{position:absolute;top:-6px;left:-6px;width:22px;height:22px;border-radius:50%;
  background:radial-gradient(circle at 35% 30%,#e07bff,#7a2bd6);border:1px solid #fff;
  display:flex;align-items:center;justify-content:center;font-size:12px;font-weight:800}
.controls{display:flex;gap:8px;align-items:center}
.controls button{background:#241c38;color:#ece7f6;border:1px solid #3a2f55;border-radius:8px;
  padding:8px 14px;font-size:13px;cursor:pointer}
.controls button:hover{background:#31264c}
.controls input{width:340px}
"#;

fn write_frame(
    path: &Path,
    faces_rel: &str,
    lanes: usize,
    snap: &Snapshot,
    hand_js: &str,
    title: &str,
    heading: &str,
) -> std::io::Result<()> {
    let doc = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>{heading}</title><style>{STYLE}</style></head><body>
<h1>AEONRIFT · ASCENDANCY</h1><p class="sub">{heading}</p>
<div id="app"></div>
<script>{script}
const S={snap};const HAND={hand};
drawBoard(document.getElementById('app'),S,HAND,"{title}");
</script></body></html>"#,
        script = board_script(faces_rel, lanes),
        snap = snapshots_to_js(std::slice::from_ref(snap))
            .trim_start_matches('[')
            .trim_end_matches(']'),
        hand = hand_js,
        title = esc(title),
        heading = esc(heading),
    );
    std::fs::write(path, doc)
}

fn write_replay(
    path: &Path,
    faces_rel: &str,
    replay: &MatchReplay,
    hand_js: &str,
    subtitle: &str,
) -> std::io::Result<()> {
    let doc = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>Aeonrift: Ascendancy — Match Replay</title><style>{STYLE}</style></head><body>
<h1>AEONRIFT · ASCENDANCY</h1><p class="sub">{subtitle}</p>
<div id="app"></div>
<div class="controls">
  <button id="play">⏸ Pause</button>
  <button id="step">Step ▸</button>
  <input id="scrub" type="range" min="0" max="{last}" value="0"/>
  <span id="counter"></span>
</div>
<script>{script}
const SNAPS={snaps};const HAND={hand};
let i=0,playing=true;
const app=document.getElementById('app');
const scrub=document.getElementById('scrub');
const counter=document.getElementById('counter');
const playBtn=document.getElementById('play');
function show(){{drawBoard(app,SNAPS[i],HAND,"Beat "+(i+1)+"/"+SNAPS.length);
  scrub.value=i;counter.textContent="tick "+SNAPS[i].t;}}
function next(){{if(i<SNAPS.length-1){{i++;show();}}else{{playing=false;playBtn.textContent="↻ Replay";}}}}
playBtn.onclick=()=>{{if(i>=SNAPS.length-1){{i=0;}}playing=!playing;playBtn.textContent=playing?"⏸ Pause":"▶ Play";}};
document.getElementById('step').onclick=()=>{{playing=false;playBtn.textContent="▶ Play";next();}};
scrub.oninput=()=>{{playing=false;playBtn.textContent="▶ Play";i=+scrub.value;show();}};
show();setInterval(()=>{{if(playing)next();}},900);
</script></body></html>"#,
        script = board_script(faces_rel, replay.config.lanes),
        snaps = snapshots_to_js(&replay.snapshots),
        hand = hand_js,
        last = replay.snapshots.len().saturating_sub(1),
        subtitle = esc(subtitle),
    );
    std::fs::write(path, doc)
}

fn main() -> ExitCode {
    let mut cfg = MatchConfig::default();
    let mut interval = DEFAULT_DEPLOY_INTERVAL;
    let mut snap_every: u64 = 240;
    let mut deck0_name = "greek-olympians".to_string();
    let mut deck1_name = "norse-aesir".to_string();
    let mut out_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/replay"));

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let val = args.next();
        match (flag.as_str(), val) {
            ("--seed", Some(v)) => cfg.seed = v.parse().unwrap_or(cfg.seed),
            ("--interval", Some(v)) => interval = v.parse().unwrap_or(interval),
            ("--snap", Some(v)) => snap_every = v.parse().unwrap_or(snap_every),
            ("--ticks", Some(v)) => cfg.max_ticks = v.parse().unwrap_or(cfg.max_ticks),
            ("--deck0", Some(v)) => deck0_name = v,
            ("--deck1", Some(v)) => deck1_name = v,
            ("--out", Some(v)) => out_dir = PathBuf::from(v),
            (other, _) => {
                eprintln!("unknown or incomplete flag: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let deck0 = match load_deck(&deck0_name) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let deck1 = match load_deck(&deck1_name) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let hand: Vec<HandCard> = deck0
        .iter()
        .map(|c| HandCard {
            slug: c.slug.clone(),
            name: c.card.name().to_string(),
            cost: c.card.meta.cost,
        })
        .collect();
    let hand_js = hand_to_js(&hand);

    let replay = record_match(cfg, deck0, deck1, interval, snap_every);

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("failed to create {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }
    let faces_rel = rel_path(&out_dir, Path::new(CARD_FACES_DIR));

    let subtitle = format!(
        "seed {} · {} vs {} · {} beats · {} — final hash {}",
        cfg.seed,
        deck0_name,
        deck1_name,
        replay.snapshots.len(),
        match replay.outcome {
            Outcome::Winner(p) => format!("player {p} wins at tick {}", replay.final_tick),
            Outcome::Draw => format!("draw at tick {}", replay.final_tick),
        },
        replay.final_hash,
    );

    // REPLAY.html — the interactive, self-driving match view.
    let replay_path = out_dir.join("REPLAY.html");
    if let Err(e) = write_replay(&replay_path, &faces_rel, &replay, &hand_js, &subtitle) {
        eprintln!("failed to write REPLAY.html: {e}");
        return ExitCode::FAILURE;
    }

    // Standalone frames: opening, two evenly-spaced beats, and the finish.
    let n = replay.snapshots.len();
    let picks: Vec<usize> = if n <= 4 {
        (0..n).collect()
    } else {
        vec![0, n / 3, (2 * n) / 3, n - 1]
    };
    let mut frame_paths = Vec::new();
    for (k, &idx) in picks.iter().enumerate() {
        let snap = &replay.snapshots[idx];
        let heading = format!(
            "Frame {} — beat {}/{} · tick {}",
            k + 1,
            idx + 1,
            n,
            snap.tick
        );
        let title = format!("Beat {}/{}", idx + 1, n);
        let fp = out_dir.join(format!("turn_{k:02}.html"));
        if let Err(e) = write_frame(
            &fp,
            &faces_rel,
            replay.config.lanes,
            snap,
            &hand_js,
            &title,
            &heading,
        ) {
            eprintln!("failed to write {}: {e}", fp.display());
            return ExitCode::FAILURE;
        }
        frame_paths.push(fp);
    }

    // BOARD.html — the static mobile board/HUD mockup (the busiest beat).
    let busiest = replay
        .snapshots
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| s.units.len())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let board_path = out_dir.join("BOARD.html");
    if let Err(e) = write_frame(
        &board_path,
        &faces_rel,
        replay.config.lanes,
        &replay.snapshots[busiest],
        &hand_js,
        "Live Board",
        "Board / HUD mockup — the busiest beat, real card faces",
    ) {
        eprintln!("failed to write BOARD.html: {e}");
        return ExitCode::FAILURE;
    }

    println!("Aeonrift: Ascendancy — headless match-replay gallery");
    println!("{subtitle}");
    println!("wrote:");
    println!("  {}", replay_path.display());
    println!("  {}", board_path.display());
    for fp in &frame_paths {
        println!("  {}", fp.display());
    }
    ExitCode::SUCCESS
}

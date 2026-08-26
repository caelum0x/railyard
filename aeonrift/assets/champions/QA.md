# Aeonrift Champion 3D Models — Hunyuan3D-2.1 HQ Re-generation QA

**Generator:** `ndreca/hunyuan3d-2.1:895e514f953d39e8b5bfb859df9313481ad3fa3a8631e5c54c7e5c9c85a6aa9f`
**Fallback:** `firtoz/trellis:e8f6c45206993f297372f5436b90350817bd9b4a0d52d2a76df50c1c8afa2b3c` (existing TRELLIS meshes)
**Render rig:** headless Blender EEVEE, 768×1024, 3-point lighting — identical to the existing gallery previews (`render_preview.py`, copied verbatim from the A/B pipeline).
**Tri budget:** `max_facenum=40000` — every generated mesh landed at ~40k tris (achilles…medusa all 40000; artemis 39994). All within budget.
**glTF validation:** every shipped `model.glb` passes the same structural assertion as the Rust `every_spec_has_a_resolvable_valid_glb` test (magic `glTF`, version 2, declared length == file size, first chunk JSON).

## Outcome summary

| Category | Count |
|---|---|
| Hunyuan3D-2.1 shipped | **17** |
| TRELLIS fallback (quality — Hunyuan clearly worse) | **4** |
| TRELLIS fallback (not regenerated — out of credit) | **26** |
| **Total champions** | **47** |

- **Hunyuan models actually generated:** 21 / 47 before the account hit a hard `402 Insufficient credit`.
- **Spend:** ~$5.67 (21 × $0.27). The run stopped **gracefully** at champion 22 (mercury) per the "stop rather than fail hard" instruction — it did not thrash the throttled API.
- Of the 21 generated, **17 passed QA and ship as Hunyuan**; **4 were clearly worse than their TRELLIS baseline and were reverted** to TRELLIS.
- The remaining **26 champions keep TRELLIS purely because they were never regenerated** (no credit), *not* a quality judgement. They are the priority list for a follow-up run once credit is topped up.

Every champion keeps its original TRELLIS mesh as `model.trellis.glb` (backup), regardless of what ships as `model.glb`.

---

## Generated champions (21) — PASS / FLAG

### PASS — shipped as Hunyuan (11)
| Champion | Faction | Verdict |
|---|---|---|
| achilles | Greek | PASS — clean Spartan; spear, shield, cape, helm all coherent |
| athena | Greek | PASS — wings + owl staff + shield captured; minor: lower legs slightly pink/under-shaded |
| bellona | Roman | PASS — plumed helm, cape, sword, armor all clean |
| brackmoor | Tide | PASS — twin-trident sea-demon; minor ice shards at feet |
| dregmaw | Umbral | PASS — spiked maw monster; minor white base patches |
| freyja | Norse | PASS — excellent; fur-collared cloak, gold trim |
| hades | Greek | PASS — antlered gold-and-blue armor; minor white ground blob at cape base |
| horus | Egyptian | PASS — falcon head, full wings, gold collar; clean oval base |
| isis | Egyptian | PASS — excellent; ankh staff, golden wings, sun disk |
| janus | Roman | PASS — clean general; red cape, gold armor, sword |
| loki | Norse | PASS — excellent; antlered green-and-gold armor, sword, cape |

### FLAG — defect noted, still shipped as Hunyuan (6)
*(TRELLIS baseline is comparably flawed, so Hunyuan is not "clearly worse" — kept per Hunyuan-default rule.)*
| Champion | Faction | FLAG reason | Why still Hunyuan |
|---|---|---|---|
| anubis-warden-of-scales | Egyptian | Right hand frozen in a **thumbs-up** gesture (the exact artifact called out in the brief) | Otherwise high-fidelity; body/scales/props clean |
| artemis | Greek | Torn/melted **cloth shards** on skirt hem; left arm crosses the face awkwardly | TRELLIS artemis has a melted bow + stringy hair — a wash |
| cyranth | Tide | Flat **white backing-sheet plane** reconstructed behind the figure | TRELLIS cyranth has a melted-out hole through the torso — worse |
| kaeza | Ember | Floating **white backing-sheet shards** + melted bow ribbon | TRELLIS kaeza's bow is fully exploded into flying shards — a wash |
| korrathil | Umbral | **White untextured ghost-sheets** haloing the tentacles + base flame sheet | TRELLIS korrathil has a large white melted blob in the torso — a wash |
| mars | Roman | Flat **pink ground-plate shards** under each foot + small gray shard by sword tip | Body/armor excellent; artifact confined to the base |

### FLAG — Hunyuan clearly worse → reverted to TRELLIS (4)
| Champion | Faction | FLAG reason | Decision |
|---|---|---|---|
| bastet-claws-of-the-hunt | Egyptian | Figure planted on a **large melted white slab/board**; dominates the silhouette | TRELLIS bastet is clean and well-textured → **keep TRELLIS** |
| gorehoof | Verdant | **Large white untextured backing sheet** behind the head + torn sheets at the base | TRELLIS gorehoof is clean and appealing → **keep TRELLIS** |
| lumisel | Tide | Figure trapped inside an **L-shaped diorama box** (floor + wall planes); lowest detail of the batch (3.8 MB) | TRELLIS lumisel is clean with a nice lily base → **keep TRELLIS** |
| medusa | Greek | **Huge white melted shroud** engulfs the whole back of the figure | TRELLIS medusa is clean naga with coiled tail → **keep TRELLIS** |

---

## Not regenerated — TRELLIS fallback due to exhausted credit (26)

These were **never sent to Hunyuan** (the account ran out of credit at champion 22). They ship on their existing TRELLIS `model.glb` unchanged. Not a quality verdict — regenerate these first when credit is available.

`mercury` (hit the 402 mid-attempt), `mosscloak`, `neptune`, `nerreth`, `nyxralla`, `odin`, `petalsong`, `pyromancer`, `quorvane`, `ra-radiant-dawn`, `sentinel`, `set-lord-of-chaos`, `sporeveil`, `thor`, `thornquill`, `toran`, `tyr`, `ullr`, `vanguard`, `vashka`, `veyl`, `vulcan`, `xelphine`, `ysmera`, `zeryth`, `zeus`.

> Note: **zeus** (whose lightning-bolt prop-glitch was the motivating example in the brief) was **not** regenerated — it remains on TRELLIS. It is in the follow-up queue.

---

## Recurring Hunyuan failure mode

The dominant artifact across the flagged champions is a **flat white/untextured "backing sheet" or base slab** — Hunyuan reconstructing the light background of the concept `reference.png` as real geometry. It is worst on references with large bright negative space (bastet, lumisel, medusa, gorehoof) and merely cosmetic where the character fills the frame. A background-matting / alpha pre-pass on the reference before the next run would likely eliminate most FLAGs.

# Aeonrift Champion 3D Models — Hunyuan3D-2.1 HQ Re-generation QA

**Generator:** `ndreca/hunyuan3d-2.1:895e514f953d39e8b5bfb859df9313481ad3fa3a8631e5c54c7e5c9c85a6aa9f`
**Fallback:** `firtoz/trellis:e8f6c45206993f297372f5436b90350817bd9b4a0d52d2a76df50c1c8afa2b3c` (existing TRELLIS meshes)
**Render rig:** headless Blender EEVEE, 768×1024, 3-point lighting — identical to the existing gallery previews (`render_preview.py`, copied verbatim from the A/B pipeline).
**Tri budget:** `max_facenum=40000` — every regenerated mesh landed at 40000 tris. All within budget.
**glTF validation:** every shipped `model.glb` passes the same structural assertion as the Rust `every_spec_has_a_resolvable_valid_glb` test (magic `glTF`, version 2, declared length == file size, first chunk JSON). **Asset test: PASS (47/47).**
**Background-matte pre-pass:** before generation, each reference was matted (`rembg` u2net → subject composited onto solid neutral `#808080` → `reference.matte.png`) so Hunyuan sees a single clean subject on a plain background. This was added specifically to kill the flat white/colored **base-geometry artifact** that dominated the first (raw-reference) pass — and it worked for the large majority.

## Outcome summary (all 47 complete)

| Category | Count |
|---|---|
| **Hunyuan3D-2.1 shipped** | **38** |
| **TRELLIS fallback (quality — Hunyuan clearly worse)** | **9** |
| **Total champions** | **47** |

- **All 47 champions were generated on Hunyuan** across three runs (51 generations total — the 4 first-pass quality-reverts were re-generated with the matte pre-pass).
- **38 ship as Hunyuan** (25 PASS, 13 FLAG-but-shipped); **9 revert to TRELLIS** where the Hunyuan mesh has a dominant white artifact and the TRELLIS baseline is clearly cleaner.
- **Spend this run:** **$1.62** (6 champions × $0.27 — veyl, vulcan, xelphine, ysmera, zeryth, zeus). **Cumulative across all runs:** ~$13.77 (51 × $0.27). Credit was **not** exhausted this run; the earlier stop was a `402` in run 1 and a transient network drop at the tail of run 2.
- Every champion keeps its original TRELLIS mesh as `model.trellis.glb` (backup), regardless of what ships as `model.glb`.

### ZEUS — the priority case ✅ CLEAN

The lightning-bolt prop caused a **white-shard glitch** in the original TRELLIS / first-Hunyuan attempts (an exploded white shard poked out of the forearm). The **matte-prepass Hunyuan zeus is clean**: no white-base artifact, coherent gold armor / red cape / white robe, and the lightning bolt renders as a modest thin white bolt held in-hand — clearly better than the TRELLIS baseline. **Ships as Hunyuan.**
Preview: `zeus/preview.hunyuan.png` (source `tools/aeonrift_3d_hq/out/zeus/preview.png`).

---

## Shipped as Hunyuan — PASS (25)

**First pass, unchanged (11):** achilles, athena, bellona, brackmoor, dregmaw, freyja, hades, horus-skyfall-talon, isis-mother-of-magic, janus, loki.

**Matte era (14):**
| Champion | Faction | Verdict |
|---|---|---|
| lumisel | Tide | PASS — matte fixed the diorama-box; clean nymph on a lily-pad base (**flipped from TRELLIS**) |
| medusa | Greek | PASS — clean gorgon, on-model green snake-hair + coiled serpent tail (**flipped from TRELLIS**) |
| mercury | Roman | PASS — winged messenger-warrior; shield, staff, feathered wings all coherent |
| neptune | Roman | PASS — trident, plumed helm, gold armor; minor pink skin under-shading |
| odin | Norse | PASS — excellent; twin ravens, fur cape, staff; minor white cape-tip |
| pyromancer | Ember | PASS — excellent; hooded fire-mage, textured flame aura on a fiery base disc |
| quorvane | Umbral | PASS — clean hooded cloak + ornate staff (hollow-hood is stylistic) |
| sporeveil | Verdant | PASS — excellent; mushroom-wizard, wide hat, dripping spore-staffs |
| toran | Ember | PASS* — dark robe + fiery base excellent (*face reads as shadowed void under topknot — see FLAG note) |
| tyr | Norse | PASS — crystalline ice-warrior; shield, fur mantle (white fur reads as fur) |
| ullr | Norse | PASS — clean ice-hunter; winged helm, ice bow, fur ruff |
| vanguard | Roman | PASS — clean Spartan; plumed helm, gold armor, black shield; tiny white foot-patch |
| vulcan | Roman | PASS — excellent smith; plumed helm, war-hammer, red cape; minor coppery base-plate |
| ysmera | Ember | PASS — excellent; dark gown with molten-ember cracks, flowing train; no artifacts |

## Shipped as Hunyuan — FLAG (defect noted, still shipped) (13)

*(Kept per Hunyuan-default rule: the TRELLIS baseline is comparably flawed, so Hunyuan is not "clearly worse.")*

**First pass, unchanged (6):** anubis-warden-of-scales (thumbs-up hand), artemis (skirt cloth-shards), cyranth (white backing-plane), kaeza (white shards + melted bow), korrathil (white ghost-sheets), mars (pink ground-plate shards).

**Matte era (7):**
| Champion | Faction | FLAG reason | Why still Hunyuan |
|---|---|---|---|
| veyl | Tide | Thin **white untextured edge-fringing** on fins/blades | TRELLIS veyl has a white crotch/base blob — a wash |
| xelphine | Umbral | Small **white melted base-blob** + pink hand-crystals | Strong hooded sorceress + moon-disc otherwise; TRELLIS comparable |
| zeryth | Umbral | **White untextured melted base/hem** shards | TRELLIS zeryth has the same flared white-shard base — a wash |
| nyxralla | Tide | **White untextured patches** on robe/staff | TRELLIS nyxralla has equal translucent/white ice bits — a wash |
| petalsong | Verdant | **White stringy shards** on harp-strings + veil | Dress/figure clean; artifact confined to props |
| set-lord-of-chaos | Egyptian | **Torn white/orange cloth shards** on right hip-sash | Body/tattoos clean and on-model |
| toran | Ember | **Featureless dark void** where the face should be (shadowed) | Robe + molten base excellent |

## Reverted to TRELLIS — Hunyuan clearly worse (9)

*(Matte pre-pass reduced but did not eliminate a dominant white artifact; the TRELLIS baseline is clearly cleaner.)*

| Champion | Faction | Hunyuan FLAG | Decision |
|---|---|---|---|
| bastet-claws-of-the-hunt | Egyptian | Residual **white base-plate** under the feet | TRELLIS clean cat-woman → **keep TRELLIS** |
| gorehoof | Verdant | **White untextured melt** on antlers + lower legs | TRELLIS clean deer-demon → **keep TRELLIS** |
| ra-radiant-dawn | Egyptian | Sun-disc reconstructed as a flat **white square billboard** + white wing | TRELLIS renders a proper glowing sun disc + clean wings → **keep TRELLIS** |
| thornquill | Verdant | Thorn-bow **exploded into white shards** (both sides) | TRELLIS clean brown thorn-bow → **keep TRELLIS** |
| sentinel | Umbral | **White base-plate** + white cape-shards | TRELLIS clean gunner, magenta cape, dark base → **keep TRELLIS** |
| thor | Norse | Large flat **white untextured cape** + hole in the hammer | TRELLIS clean red/pink cape → **keep TRELLIS** |
| mosscloak | Verdant | **White base-plate** under the feet | TRELLIS clean plant-beast → **keep TRELLIS** |
| vashka | Umbral | Prominent **floating white banner billboard** upper-right | TRELLIS clean armor + small orange torch → **keep TRELLIS** |
| nerreth | Umbral | **White flame-sheet billboard** behind the head | TRELLIS clean golden-flame head + detailed body → **keep TRELLIS** |

---

## Recurring Hunyuan failure mode & the matte fix

The dominant first-pass artifact was a **flat white "backing sheet" / base slab** — Hunyuan reconstructing the bright background of the concept `reference.png` as real geometry. The **background-matte pre-pass** (subject on neutral `#808080`) eliminated it for the large majority: most champions now render as a clean subject on the dark preview background with no base slab. It persists only where a **complex prop or backdrop** (a sun disc, a thorny bow, a flame aura, a banner) sits in bright negative space and gets reconstructed as an untextured white billboard/shard — those 9 are the TRELLIS reverts above. Zeus, the motivating prop-glitch case, came out clean.

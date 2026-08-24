# Character Lock Passport (working name)

> Supersedes the original "AI context portability / OAuth identity" concept in PLAN.md.
> This is now a **Railyard infra service** — a provider-agnostic identity & consistency layer
> consumed by *any* product on the rail (Menivor, and future payers), across *all* AI model providers.

## What it is
A **portable, locked "passport"** for an AI-generated identity — a character, brand, or persona — that
travels across every model provider and every product. Lock it once; reuse it everywhere with
byte-stable consistency.

## What it locks (the passport fields, versioned + immutable once locked)
- **Character:** face/appearance reference images, voice ID (ElevenLabs/etc), avatar/LoRA/embedding refs,
  body/wardrobe, style, personality/tone.
- **Brand:** logo, palette, typography, watermark, voice-and-tone guidelines, disclaimers.
- **Personalization:** per-audience/segment preferences, language, target platform conventions, hooks that
  convert for this persona (fed by the Hook Performance Loop).

## Why it's Railyard infra (not a Menivor feature)
- **Provider-agnostic:** resolves a locked passport into the right conditioning for *any* provider —
  routed through **Relay** (image/video/voice/text). Same character on Veo, Runway, Kling, HeyGen,
  ElevenLabs, or a local LoRA — one passport, consistent output.
- **Cross-product:** Menivor uses it for consistent UGC reels; other rail products reuse the same passport
  for consistent marketing + personalized generation. The passport is the durable, reusable asset — like Mint.
- **Metered on the rail:** passport-conditioned generations settle through Mint like any other rail spend.

## Core surface (to design)
- `create/version/lock` a passport; immutable once locked, new versions are explicit.
- `resolve(passport_id, provider, modality)` → provider-specific conditioning payload (via Relay adapters).
- `generate AS passport X` — the render/marketing pipeline requests output conditioned on a locked passport.
- audit + provenance: which passport produced which asset.

## Relationship to the rest of Railyard
- **Relay** does the provider fan-out; the Passport supplies the per-provider conditioning.
- **Mint** settles the spend.
- Consumed by Menivor (video), and any future product needing consistent characters/brand/personalization.
